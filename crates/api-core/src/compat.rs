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

use ::rpc::forge::ForgeAgentControlResponse;
use ::rpc::{forge as rpc, forge_agent_control_response as fac};
use carbide_host_support::dpa_cmds::DpaCommand;
use model::machine::MachineValidationFilter;
use rpc::forge_agent_control_response::forge_agent_control_extra_info::KeyValuePair;

use crate::CarbideError;
use crate::errors::CarbideResult;

/// Supports building a type from a source value while also filling in all legacy fields.
pub(crate) trait BuildAndFillLegacyFields {
    type Source;

    fn build_and_fill_legacy_fields(source: Self::Source) -> CarbideResult<Self>
    where
        Self: Sized;
}

/// ForgeAgentControlResponse has been migrated from including extra information as key/value string
/// pairs in a `data` field, to having strongly-typed values for the `action` field. There is a
/// possibility that scout instances will still be running while we update carbide-api, so we need
/// to "dual write" the fields in ForgeAgentControlResponse, until scout is upgraded everywhere. At
/// that point we can delete this code and construct ForgeAgentControlResponse from an Action
/// directly using the From trait.
impl BuildAndFillLegacyFields for ForgeAgentControlResponse {
    type Source = fac::Action;

    fn build_and_fill_legacy_fields(action: fac::Action) -> CarbideResult<Self> {
        let (legacy_action, key_values) = match &action {
            fac::Action::Noop(_) => (fac::LegacyAction::Noop, None),
            fac::Action::Reset(_) => (fac::LegacyAction::Reset, None),
            fac::Action::Discovery(_) => (fac::LegacyAction::Discovery, None),
            fac::Action::Rebuild(_) => (fac::LegacyAction::Rebuild, None),
            fac::Action::Retry(_) => (fac::LegacyAction::Retry, None),
            fac::Action::Measure(_) => (fac::LegacyAction::Measure, None),
            fac::Action::LogError(_) => (fac::LegacyAction::Logerror, None),
            fac::Action::MachineValidation(machine_validation) => {
                let fac::MachineValidation {
                    context,
                    validation_id,
                    filter,
                    is_enabled,
                } = machine_validation;
                let Some(validation_id) = validation_id else {
                    return Err(CarbideError::internal("MachineValidation action is missing validation_id".to_string()));
                };
                // Note: Some tests are sensitive to ordering here, put ValidationId second.
                let mut pairs = vec![
                    KeyValuePair {
                        key: "Context".to_string(),
                        value: context.clone(),
                    },
                    KeyValuePair {
                        key: "ValidationId".to_string(),
                        value: validation_id.to_string(),
                    },
                    KeyValuePair {
                        key: "IsEnabled".to_string(),
                        value: is_enabled.to_string(),
                    },
                ];
                if let Some(filter) = filter {
                    pairs.push(KeyValuePair {
                        key: "MachineValidationFilter".to_string(),
                        value: serde_json::to_string(&MachineValidationFilter::from(filter.clone()))?,
                    });
                }
                (fac::LegacyAction::MachineValidation, Some(pairs))
            }
            fac::Action::MlxAction(mlx_action) => (
                fac::LegacyAction::MlxAction,
                Some(
                    mlx_action
                        .device_actions
                        .iter()
                        .filter_map(|action| Some((action, action.command.as_ref()?)))
                        .map(|(action, command)| {
                            Ok::<_, CarbideError>(KeyValuePair {
                                key: action.pci_name.clone(),
                                value: serde_json::to_string(
                                    &DpaCommand::try_from(command.clone()).map_err(|e| {
                                        CarbideError::internal(format!(
                                            "error converting MlxAction to JSON for legacy fields: {e}"
                                        ))
                                    })?,
                                )?,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            ),
            fac::Action::FirmwareUpgrade(firmware_upgrade) => (
                fac::LegacyAction::FirmwareUpgrade,
                firmware_upgrade
                    .task
                    .as_ref()
                    .map(|task| {
                        Ok::<_, CarbideError>(vec![KeyValuePair {
                            key: "firmware_upgrade_task".to_string(),
                            value: serde_json::to_string(task)?,
                        }])
                    })
                    .transpose()?,
            ),
        };
        Ok(ForgeAgentControlResponse {
            action: Some(action),
            legacy_action: legacy_action as i32,
            data: key_values.map(|pair| fac::ForgeAgentControlExtraInfo { pair }),
        })
    }
}

#[cfg(test)]
mod tests {
    use ::rpc::protos::mlx_device;
    use ::rpc::{common, scout_firmware_upgrade as sfu};
    use carbide_uuid::machine_validation::MachineValidationId;

    use super::*;

    #[test]
    fn response_from_typed_action_sets_typed_and_legacy_fields() {
        let action = fac::Action::discovery();

        let response = ForgeAgentControlResponse::build_and_fill_legacy_fields(action).unwrap();

        assert_eq!(response.legacy_action, fac::LegacyAction::Discovery as i32);
        assert!(matches!(response.action, Some(fac::Action::Discovery(_))));
        assert!(response.data.is_none());
    }

    #[test]
    fn machine_validation_converts_to_legacy_pairs() {
        let validation_id = MachineValidationId::new();
        let action = fac::Action::MachineValidation(fac::MachineValidation {
            is_enabled: true,
            context: "Discovery".to_string(),
            validation_id: Some(validation_id),
            filter: Some(fac::MachineValidationFilter {
                tags: vec!["smoke".to_string()],
                allowed_tests: vec!["test-a".to_string()],
                run_unverfied_tests: Some(true),
                contexts: Some(common::StringList {
                    items: vec!["ctx-a".to_string()],
                }),
            }),
        });

        let response = ForgeAgentControlResponse::build_and_fill_legacy_fields(action).unwrap();
        let data = response.data.expect("legacy data");
        let filter = data
            .pair
            .iter()
            .find(|pair| pair.key == "MachineValidationFilter")
            .expect("filter");
        let is_enabled = data
            .pair
            .iter()
            .find_map(|pair| {
                (pair.key == "IsEnabled")
                    .then(|| serde_json::from_str::<bool>(&pair.value).unwrap())
            })
            .expect("is_enabled");

        assert_eq!(
            response.legacy_action,
            fac::LegacyAction::MachineValidation as i32
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&filter.value).unwrap(),
            serde_json::json!({
                "tags": ["smoke"],
                "allowed_tests": ["test-a"],
                "run_unverfied_tests": true,
                "contexts": ["ctx-a"],
            })
        );
        assert!(is_enabled);
    }

    #[test]
    fn response_from_machine_validation_sets_typed_payload_and_legacy_pairs() {
        let validation_id = MachineValidationId::new();
        let response = ForgeAgentControlResponse::build_and_fill_legacy_fields(
            fac::Action::MachineValidation(fac::MachineValidation {
                is_enabled: true,
                context: "Discovery".to_string(),
                validation_id: Some(validation_id),
                filter: Some(fac::MachineValidationFilter {
                    tags: vec!["smoke".to_string()],
                    allowed_tests: vec!["test-a".to_string()],
                    run_unverfied_tests: Some(true),
                    contexts: Some(common::StringList {
                        items: vec!["ctx-a".to_string()],
                    }),
                }),
            }),
        )
        .unwrap();

        let Some(fac::Action::MachineValidation(machine_validation)) = response.action.as_ref()
        else {
            panic!("expected typed machine validation action");
        };
        let legacy_data = response.data.as_ref().expect("legacy data");

        assert_eq!(
            response.legacy_action,
            fac::LegacyAction::MachineValidation as i32
        );
        assert_eq!(machine_validation.context, "Discovery");
        assert_eq!(machine_validation.validation_id.unwrap(), validation_id,);
        assert_eq!(
            machine_validation
                .filter
                .as_ref()
                .unwrap()
                .allowed_tests
                .as_slice(),
            ["test-a"]
        );
        println!("Legacy data: {:?}", legacy_data);
        assert!(
            legacy_data
                .pair
                .iter()
                .any(|pair| pair.key == "ValidationId" && pair.value == validation_id.to_string())
        );
        assert!(
            legacy_data
                .pair
                .iter()
                .find_map(|pair| {
                    (pair.key == "IsEnabled")
                        .then(|| serde_json::from_str::<bool>(&pair.value).unwrap())
                })
                .expect("is_enabled")
        );
    }

    #[test]
    fn mlx_action_converts_to_legacy_dpa_command_json() {
        let action = fac::Action::MlxAction(fac::MlxAction {
            device_actions: vec![fac::MlxDeviceAction {
                pci_name: "04:00.0".to_string(),
                command: Some(fac::mlx_device_action::Command::ApplyProfile(
                    fac::MlxDeviceApplyProfile {
                        serialized_profile: Some(mlx_device::SerializableMlxConfigProfile {
                            name: "default".to_string(),
                            registry_name: "bf3".to_string(),
                            description: None,
                            config: [("SRIOV_EN".to_string(), "true".to_string())].into(),
                        }),
                    },
                )),
            }],
        });

        let response = ForgeAgentControlResponse::build_and_fill_legacy_fields(action).unwrap();
        let pair = &response.data.as_ref().expect("legacy data").pair[0];

        assert_eq!(response.legacy_action(), fac::LegacyAction::MlxAction);
        assert_eq!(pair.key, "04:00.0");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pair.value).unwrap(),
            serde_json::json!({
                "op": {
                    "ApplyProfile": {
                        "serialized_profile": {
                            "name": "default",
                            "registry_name": "bf3",
                            // description is skipped if None
                            "config": { "SRIOV_EN": true },
                        },
                    },
                },
            })
        );
    }

    #[test]
    fn response_from_mlx_action_sets_typed_payload_and_legacy_pairs() {
        let response = ForgeAgentControlResponse::build_and_fill_legacy_fields(
            fac::Action::MlxAction(fac::MlxAction {
                device_actions: vec![fac::MlxDeviceAction {
                    pci_name: "04:00.0".to_string(),
                    command: Some(fac::mlx_device_action::Command::Lock(fac::MlxDeviceLock {
                        key: "secret".to_string(),
                    })),
                }],
            }),
        )
        .unwrap();

        let Some(fac::Action::MlxAction(mlx_action)) = response.action.as_ref() else {
            panic!("expected typed mlx action");
        };
        let legacy_pair = &response.data.as_ref().expect("legacy data").pair[0];

        assert_eq!(response.legacy_action(), fac::LegacyAction::MlxAction);
        assert_eq!(mlx_action.device_actions[0].pci_name, "04:00.0");
        assert!(matches!(
            mlx_action.device_actions[0].command.as_ref(),
            Some(fac::mlx_device_action::Command::Lock(fac::MlxDeviceLock { key }))
                if key == "secret"
        ));
        assert_eq!(legacy_pair.key, "04:00.0");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&legacy_pair.value).unwrap(),
            serde_json::json!({
                "op": {
                    "Lock": {
                        "key": "secret",
                    },
                },
            })
        );
    }

    #[test]
    fn firmware_upgrade_converts_to_legacy_task_json() {
        let upgrade_task_id = uuid::Uuid::new_v4().to_string();
        let action = fac::Action::FirmwareUpgrade(fac::FirmwareUpgrade {
            task: Some(sfu::ScoutFirmwareUpgradeTask {
                upgrade_task_id: upgrade_task_id.clone(),
                component_type: "cpld".to_string(),
                target_version: "1.2.3".to_string(),
                script: Some(sfu::FileArtifact {
                    url: "http://pxe/script.sh".to_string(),
                    sha256: "abc".to_string(),
                }),
                execution_timeout_seconds: 30,
                artifact_download_timeout_seconds: 10,
                file_artifacts: vec![sfu::FileArtifact {
                    url: "http://pxe/fw.bin".to_string(),
                    sha256: "def".to_string(),
                }],
            }),
        });

        let response = ForgeAgentControlResponse::build_and_fill_legacy_fields(action).unwrap();
        let pair = &response.data.as_ref().expect("legacy data").pair[0];

        assert_eq!(response.legacy_action(), fac::LegacyAction::FirmwareUpgrade);
        assert_eq!(pair.key, "firmware_upgrade_task");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pair.value).unwrap(),
            serde_json::json!({
                "upgrade_task_id": upgrade_task_id,
                "component_type": "cpld",
                "target_version": "1.2.3",
                "script": {
                    "url": "http://pxe/script.sh",
                    "sha256": "abc",
                },
                "execution_timeout_seconds": 30,
                "artifact_download_timeout_seconds": 10,
                "file_artifacts": [{
                    "url": "http://pxe/fw.bin",
                    "sha256": "def",
                }],
            })
        );
    }

    #[test]
    fn response_from_firmware_upgrade_sets_typed_payload_and_legacy_pairs() {
        let response = ForgeAgentControlResponse::build_and_fill_legacy_fields(
            fac::Action::FirmwareUpgrade(fac::FirmwareUpgrade {
                task: Some(sfu::ScoutFirmwareUpgradeTask {
                    upgrade_task_id: uuid::Uuid::new_v4().to_string(),
                    component_type: "cpld".to_string(),
                    target_version: "1.2.3".to_string(),
                    script: Some(sfu::FileArtifact {
                        url: "http://pxe/script.sh".to_string(),
                        sha256: "abc".to_string(),
                    }),
                    execution_timeout_seconds: 30,
                    artifact_download_timeout_seconds: 10,
                    file_artifacts: vec![sfu::FileArtifact {
                        url: "http://pxe/fw.bin".to_string(),
                        sha256: "def".to_string(),
                    }],
                }),
            }),
        )
        .unwrap();

        let Some(fac::Action::FirmwareUpgrade(firmware_upgrade)) = response.action.as_ref() else {
            panic!("expected typed firmware upgrade action");
        };
        let task = firmware_upgrade.task.as_ref().expect("typed task");
        let legacy_pair = &response.data.as_ref().expect("legacy data").pair[0];

        assert_eq!(response.legacy_action(), fac::LegacyAction::FirmwareUpgrade,);
        assert_eq!(task.component_type, "cpld");
        assert_eq!(task.script.as_ref().unwrap().url, "http://pxe/script.sh");
        assert_eq!(legacy_pair.key, "firmware_upgrade_task");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&legacy_pair.value).unwrap()["target_version"],
            "1.2.3"
        );
    }
}
