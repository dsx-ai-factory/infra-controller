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
use std::str::FromStr;

use carbide_utils::none_if_empty::NoneIfEmpty;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use model::machine_validation::{
    MachineValidation, MachineValidationAttempt, MachineValidationExternalConfig,
    MachineValidationPlugin, MachineValidationResult, MachineValidationRunItem,
    MachineValidationState, MachineValidationTest, MachineValidationTestAddRequest,
    MachineValidationTestUpdatePayload, MachineValidationTestUpdateRequest,
    MachineValidationTestsGetRequest,
};

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<rpc::forge::MachineValidationTestAddRequest> for MachineValidationTestAddRequest {
    fn from(req: rpc::forge::MachineValidationTestAddRequest) -> Self {
        MachineValidationTestAddRequest {
            name: req.name,
            description: req.description,
            contexts: req.contexts,
            img_name: req.img_name,
            execute_in_host: req.execute_in_host,
            container_arg: req.container_arg,
            command: req.command,
            args: req.args,
            extra_err_file: req.extra_err_file,
            external_config_file: req.external_config_file,
            pre_condition: req.pre_condition,
            timeout: req.timeout,
            extra_output_file: req.extra_output_file,
            supported_platforms: req.supported_platforms,
            read_only: req.read_only,
            custom_tags: req.custom_tags,
            components: req.components,
            is_enabled: req.is_enabled,
            plugin: req.plugin.map(Into::into),
        }
    }
}

impl From<rpc::forge::machine_validation_test_update_request::Payload>
    for MachineValidationTestUpdatePayload
{
    fn from(p: rpc::forge::machine_validation_test_update_request::Payload) -> Self {
        MachineValidationTestUpdatePayload {
            name: p.name,
            description: p.description,
            contexts: p.contexts,
            img_name: p.img_name,
            execute_in_host: p.execute_in_host,
            container_arg: p.container_arg,
            command: p.command,
            args: p.args,
            extra_err_file: p.extra_err_file,
            external_config_file: p.external_config_file,
            pre_condition: p.pre_condition,
            timeout: p.timeout,
            extra_output_file: p.extra_output_file,
            supported_platforms: p.supported_platforms,
            verified: p.verified,
            custom_tags: p.custom_tags,
            components: p.components,
            is_enabled: p.is_enabled,
            plugin: p.plugin.map(Into::into),
        }
    }
}

impl From<rpc::forge::MachineValidationTestUpdateRequest> for MachineValidationTestUpdateRequest {
    fn from(req: rpc::forge::MachineValidationTestUpdateRequest) -> Self {
        MachineValidationTestUpdateRequest {
            test_id: req.test_id,
            version: req.version,
            payload: req.payload.map(MachineValidationTestUpdatePayload::from),
        }
    }
}

impl From<rpc::forge::MachineValidationTestsGetRequest> for MachineValidationTestsGetRequest {
    fn from(req: rpc::forge::MachineValidationTestsGetRequest) -> Self {
        MachineValidationTestsGetRequest {
            supported_platforms: req.supported_platforms,
            contexts: req.contexts,
            test_id: req.test_id,
            read_only: req.read_only,
            custom_tags: req.custom_tags,
            version: req.version,
            is_enabled: req.is_enabled,
            verified: req.verified,
        }
    }
}

pub fn machine_validation_from_state(
    state: MachineValidationState,
) -> rpc::forge::machine_validation_status::MachineValidationState {
    match state {
        MachineValidationState::Started => {
            rpc::forge::machine_validation_status::MachineValidationState::Started(
                rpc::forge::machine_validation_status::MachineValidationStarted::Started.into(),
            )
        }
        MachineValidationState::InProgress => {
            rpc::forge::machine_validation_status::MachineValidationState::InProgress(
                rpc::forge::machine_validation_status::MachineValidationInProgress::InProgress
                    .into(),
            )
        }
        MachineValidationState::Success => {
            rpc::forge::machine_validation_status::MachineValidationState::Completed(
                rpc::forge::machine_validation_status::MachineValidationCompleted::Success.into(),
            )
        }
        MachineValidationState::Skipped => {
            rpc::forge::machine_validation_status::MachineValidationState::Completed(
                rpc::forge::machine_validation_status::MachineValidationCompleted::Skipped.into(),
            )
        }
        MachineValidationState::Failed => {
            rpc::forge::machine_validation_status::MachineValidationState::Completed(
                rpc::forge::machine_validation_status::MachineValidationCompleted::Failed.into(),
            )
        }
    }
}

impl From<MachineValidation> for rpc::forge::MachineValidationRun {
    fn from(value: MachineValidation) -> Self {
        let mut end_time = None;
        if value.end_time.is_some() {
            end_time = Some(value.end_time.unwrap_or_default().into());
        }
        let status = value.status.unwrap_or_default();
        let start_time = Some(value.start_time.unwrap_or_default().into());
        rpc::forge::MachineValidationRun {
            validation_id: Some(value.id),
            name: value.name,
            start_time,
            end_time,
            context: value.context,
            machine_id: Some(value.machine_id),
            status: Some(rpc::forge::MachineValidationStatus {
                machine_validation_state: machine_validation_from_state(status.state).into(),
                total: status.total.try_into().unwrap_or(0),
                completed_tests: status.completed.try_into().unwrap_or(0),
            }),
            duration_to_complete: Some(rpc::Duration::from(std::time::Duration::from_secs(
                value.duration_to_complete.try_into().unwrap_or(0),
            ))),
            last_heartbeat_at: value.last_heartbeat_at.map(Into::into),
        }
    }
}

impl From<MachineValidationRunItem> for rpc::forge::MachineValidationRunItem {
    fn from(value: MachineValidationRunItem) -> Self {
        rpc::forge::MachineValidationRunItem {
            run_item_id: Some(rpc::common::Uuid {
                value: value.id.to_string(),
            }),
            current_attempt_id: value.current_attempt_id.map(|id| rpc::common::Uuid {
                value: id.to_string(),
            }),
            validation_id: Some(value.run_id),
            test_id: value.test_id,
            test_version: value.test_version,
            display_name: value.display_name,
            context: value.context,
            component: value.component,
            state: value.state.to_string(),
            order_index: value.order_index.try_into().unwrap_or(0),
            attempt: value.attempt.try_into().unwrap_or(0),
            max_attempts: value.max_attempts.try_into().unwrap_or(0),
            timeout: Some(rpc::Duration::from(std::time::Duration::from_secs(
                value.timeout_seconds.try_into().unwrap_or(0),
            ))),
            started_at: value.started_at.map(Into::into),
            ended_at: value.ended_at.map(Into::into),
            last_heartbeat_at: value.last_heartbeat_at.map(Into::into),
            skip_reason: value.skip_reason,
            failure_reason: value.failure_reason,
        }
    }
}

impl From<MachineValidationAttempt> for rpc::forge::MachineValidationAttempt {
    fn from(value: MachineValidationAttempt) -> Self {
        rpc::forge::MachineValidationAttempt {
            attempt_id: Some(rpc::common::Uuid {
                value: value.id.to_string(),
            }),
            run_item_id: Some(rpc::common::Uuid {
                value: value.run_item_id.to_string(),
            }),
            attempt_number: value.attempt_number.try_into().unwrap_or(0),
            state: value.state.to_string(),
            command: value.command,
            args: value.args,
            container_image: value.container_image,
            execute_in_host: value.execute_in_host,
            exit_code: value.exit_code,
            failure_classification: value.failure_classification,
            started_at: value.started_at.map(Into::into),
            ended_at: value.ended_at.map(Into::into),
            last_heartbeat_at: value.last_heartbeat_at.map(Into::into),
            stdout_summary: value.stdout_summary,
            stderr_summary: value.stderr_summary,
        }
    }
}

impl From<MachineValidationExternalConfig> for rpc::forge::MachineValidationExternalConfig {
    fn from(value: MachineValidationExternalConfig) -> Self {
        rpc::forge::MachineValidationExternalConfig {
            name: value.name,
            config: value.config,
            description: Some(value.description),
            version: value.version.version_nr().to_string(),
            timestamp: Some(value.version.timestamp().into()),
        }
    }
}

impl TryFrom<rpc::forge::MachineValidationExternalConfig> for MachineValidationExternalConfig {
    type Error = RpcDataConversionError;
    fn try_from(value: rpc::forge::MachineValidationExternalConfig) -> Result<Self, Self::Error> {
        Ok(MachineValidationExternalConfig {
            name: value.name,
            description: value.description.unwrap_or_default(),
            config: value.config,
            version: ConfigVersion::from_str(&value.version)
                .map_err(|_| RpcDataConversionError::InvalidConfigVersion(value.version))?,
        })
    }
}

impl From<MachineValidationTest> for rpc::forge::MachineValidationTest {
    fn from(value: MachineValidationTest) -> Self {
        rpc::forge::MachineValidationTest {
            test_id: value.test_id,
            name: value.name,
            description: value.description,
            contexts: value.contexts,
            img_name: value.img_name,
            execute_in_host: value.execute_in_host,
            container_arg: value.container_arg,
            command: value.command,
            args: value.args,
            extra_output_file: value.extra_output_file,
            extra_err_file: value.extra_err_file,
            external_config_file: value.external_config_file,
            pre_condition: value.pre_condition,
            timeout: value.timeout,
            version: value.version.version_string(),
            supported_platforms: value.supported_platforms,
            modified_by: value.modified_by,
            verified: value.verified,
            read_only: value.read_only,
            custom_tags: value.custom_tags.unwrap_or_default(),
            components: value.components,
            last_modified_at: value.last_modified_at.to_string(),
            is_enabled: value.is_enabled,
            plugin: value.plugin.map(Into::into),
            full_host_approved: value.full_host_approved,
        }
    }
}

impl TryFrom<rpc::forge::MachineValidationTest> for MachineValidationTest {
    type Error = RpcDataConversionError;
    fn try_from(value: rpc::forge::MachineValidationTest) -> Result<Self, Self::Error> {
        Ok(MachineValidationTest {
            test_id: value.test_id,
            name: value.name,
            description: value.description,
            contexts: value.contexts,
            img_name: value.img_name,
            execute_in_host: value.execute_in_host,
            container_arg: value.container_arg,
            command: value.command,
            args: value.args,
            extra_output_file: value.extra_output_file,
            extra_err_file: value.extra_err_file,
            external_config_file: value.external_config_file,
            pre_condition: value.pre_condition,
            timeout: value.timeout,
            version: ConfigVersion::from_str(&value.version)
                .map_err(|_| RpcDataConversionError::InvalidConfigVersion(value.version))?,
            supported_platforms: value.supported_platforms,
            modified_by: value.modified_by,
            verified: value.verified,
            read_only: value.read_only,
            custom_tags: value.custom_tags.none_if_empty(),
            components: value.components,
            last_modified_at: Utc::now(),
            is_enabled: value.is_enabled,
            plugin: value.plugin.map(Into::into),
            full_host_approved: value.full_host_approved,
        })
    }
}

impl From<rpc::forge::MachineValidationPlugin> for MachineValidationPlugin {
    fn from(value: rpc::forge::MachineValidationPlugin) -> Self {
        Self {
            image: value.image,
            entrypoint: value.entrypoint,
            parameters_json: value.parameters_json,
            privileged: value.privileged,
            host_access_full: value.host_access_full,
        }
    }
}

impl From<MachineValidationPlugin> for rpc::forge::MachineValidationPlugin {
    fn from(value: MachineValidationPlugin) -> Self {
        Self {
            image: value.image,
            entrypoint: value.entrypoint,
            parameters_json: value.parameters_json,
            privileged: value.privileged,
            host_access_full: value.host_access_full,
        }
    }
}

impl From<MachineValidationResult> for rpc::forge::MachineValidationResult {
    fn from(value: MachineValidationResult) -> Self {
        rpc::forge::MachineValidationResult {
            validation_id: Some(value.validation_id),
            command: value.command,
            args: value.args,
            std_out: value.stdout,
            std_err: value.stderr,
            name: value.name,
            description: value.description,
            context: value.context,
            exit_code: value.exit_code,
            start_time: Some(value.start_time.into()),
            end_time: Some(value.end_time.into()),
            test_id: value.test_id,
        }
    }
}

impl TryFrom<rpc::forge::MachineValidationResult> for MachineValidationResult {
    type Error = RpcDataConversionError;
    fn try_from(value: rpc::forge::MachineValidationResult) -> Result<Self, Self::Error> {
        let val_id = value
            .validation_id
            .ok_or(RpcDataConversionError::MissingArgument("validation_id"))?;
        let start_time = match value.start_time {
            Some(time) => {
                DateTime::from_timestamp(time.seconds, time.nanos.try_into().unwrap()).unwrap()
            }
            None => Utc::now(),
        };
        let end_time = match value.end_time {
            Some(time) => {
                DateTime::from_timestamp(time.seconds, time.nanos.try_into().unwrap()).unwrap()
            }
            None => Utc::now(),
        };
        Ok(MachineValidationResult {
            validation_id: val_id,
            command: value.command,
            name: value.name,
            description: value.description,
            args: value.args,
            context: value.context,
            stdout: value.std_out,
            stderr: value.std_err,
            exit_code: value.exit_code,
            start_time,
            end_time,
            test_id: value.test_id,
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn tests_get_request_from_rpc() {
        let rpc_req = rpc::forge::MachineValidationTestsGetRequest {
            test_id: Some("forge_mytest".to_string()),
            is_enabled: Some(true),
            verified: Some(false),
            ..Default::default()
        };
        let req = MachineValidationTestsGetRequest::from(rpc_req);
        assert_eq!(req.test_id, Some("forge_mytest".to_string()));
        assert_eq!(req.is_enabled, Some(true));
        assert_eq!(req.verified, Some(false));
        assert!(req.version.is_none());
    }

    #[test]
    fn test_add_request_from_rpc() {
        let rpc_req = rpc::forge::MachineValidationTestAddRequest {
            name: "my_test".to_string(),
            command: "/bin/test".to_string(),
            args: "--verbose".to_string(),
            supported_platforms: vec!["x86_64".to_string()],
            ..Default::default()
        };
        let req = MachineValidationTestAddRequest::from(rpc_req);
        assert_eq!(req.name, "my_test");
        assert_eq!(req.command, "/bin/test");
        assert_eq!(req.supported_platforms, vec!["x86_64"]);
    }

    #[test]
    fn test_update_request_from_rpc_with_payload() {
        let rpc_req = rpc::forge::MachineValidationTestUpdateRequest {
            test_id: "forge_mytest".to_string(),
            version: "1".to_string(),
            payload: Some(
                rpc::forge::machine_validation_test_update_request::Payload {
                    verified: Some(true),
                    is_enabled: Some(false),
                    ..Default::default()
                },
            ),
        };
        let req = MachineValidationTestUpdateRequest::from(rpc_req);
        assert_eq!(req.test_id, "forge_mytest");
        assert_eq!(req.version, "1");
        let payload = req.payload.unwrap();
        assert_eq!(payload.verified, Some(true));
        assert_eq!(payload.is_enabled, Some(false));
        assert!(payload.name.is_none());
    }
}
