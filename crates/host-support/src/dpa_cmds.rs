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

use std::borrow::Cow;

use libmlx::firmware::config::FirmwareFlasherProfile;
use libmlx::profile::error::MlxProfileError;
use libmlx::profile::serialization::SerializableProfile;
use rpc::forge_agent_control_response as fac;
use rpc::forge_agent_control_response::mlx_device_action::{Command as DpaCommandPb, Command};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum OpCode<'a> {
    Noop,
    Unlock {
        key: String,
    },
    ApplyProfile {
        serialized_profile: Option<SerializableProfile>,
    },
    Lock {
        key: String,
    },
    /// Apply a selected firmware profile as one identified operation.
    ApplyFirmware {
        /// The selected target, or `None` when Core requests no firmware change.
        profile: Option<Box<Cow<'a, FirmwareFlasherProfile>>>,
        /// Nonempty identifier for one persisted operation across repeated polls.
        operation_id: String,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DpaCommand<'a> {
    pub op: OpCode<'a>,
}

impl TryFrom<DpaCommandPb> for DpaCommand<'static> {
    type Error = String;

    fn try_from(value: DpaCommandPb) -> Result<Self, Self::Error> {
        Ok(Self {
            op: match value {
                Command::Noop(_) => OpCode::Noop,
                Command::Lock(lock) => OpCode::Lock { key: lock.key },
                Command::Unlock(unlock) => OpCode::Unlock { key: unlock.key },
                Command::ApplyProfile(apply_profile) => OpCode::ApplyProfile {
                    serialized_profile: apply_profile
                        .serialized_profile
                        .map(TryInto::try_into)
                        .transpose()
                        .map_err(|e: MlxProfileError| e.to_string())?,
                },
                Command::ApplyFirmware(_) => {
                    return Err(
                        "legacy ApplyFirmware command cannot identify a durable operation"
                            .to_string(),
                    );
                }
                Command::ApplyFirmwareOperation(apply_firmware) => {
                    if apply_firmware.operation_id.is_empty() {
                        return Err("firmware operation_id must not be empty".to_string());
                    }
                    OpCode::ApplyFirmware {
                        operation_id: apply_firmware.operation_id,
                        profile: apply_firmware
                            .profile
                            .map(|p| Ok::<_, String>(Box::new(Cow::Owned(p.try_into()?))))
                            .transpose()?,
                    }
                }
                Command::UpdateFirmware(_) => {
                    return Err(
                        "UpdateFirmware requires the typed NIC firmware command path".to_string(),
                    );
                }
                Command::ObserveFirmwareUpdate(_) => {
                    return Err(
                        "ObserveFirmwareUpdate requires the typed NIC firmware command path"
                            .to_string(),
                    );
                }
                Command::Lockdown(_) => {
                    return Err("Lockdown requires the typed NIC firmware command path".to_string());
                }
            },
        })
    }
}

pub struct DpaDeviceCommand<'a> {
    pub pci_name: String,
    pub command: DpaCommand<'a>,
}

impl TryFrom<DpaDeviceCommand<'_>> for fac::MlxDeviceAction {
    type Error = String;

    fn try_from(device_command: DpaDeviceCommand<'_>) -> Result<Self, Self::Error> {
        let command = match device_command.command.op {
            OpCode::Noop => fac::mlx_device_action::Command::Noop(fac::MlxDeviceNoop {}),
            OpCode::Unlock { key } => {
                fac::mlx_device_action::Command::Unlock(fac::MlxDeviceUnlock { key })
            }
            OpCode::ApplyProfile { serialized_profile } => {
                let serialized_profile = serialized_profile
                    .map(|profile| (&profile).try_into())
                    .transpose()
                    .map_err(|e: libmlx::profile::error::MlxProfileError| e.to_string())?;
                fac::mlx_device_action::Command::ApplyProfile(fac::MlxDeviceApplyProfile {
                    serialized_profile,
                })
            }
            OpCode::Lock { key } => {
                fac::mlx_device_action::Command::Lock(fac::MlxDeviceLock { key })
            }
            OpCode::ApplyFirmware {
                profile,
                operation_id,
            } => {
                if operation_id.is_empty() {
                    return Err("firmware operation_id must not be empty".to_string());
                }
                let profile = profile.map(|profile| (*profile).into_owned().into());
                fac::mlx_device_action::Command::ApplyFirmwareOperation(
                    fac::MlxDeviceApplyFirmwareOperation {
                        profile,
                        operation_id,
                    },
                )
            }
        };

        Ok(fac::MlxDeviceAction {
            pci_name: device_command.pci_name,
            command: Some(command),
        })
    }
}

impl TryFrom<&fac::MlxDeviceAction> for DpaCommand<'static> {
    type Error = String;

    fn try_from(device_action: &fac::MlxDeviceAction) -> Result<Self, Self::Error> {
        match &device_action.command {
            Some(command) => command.clone().try_into(),
            None => Ok(DpaCommand { op: OpCode::Noop }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use rpc::protos::mlx_device::{
        FirmwareFlasherProfile as FirmwareProfilePb, FirmwareSpec as FirmwareSpecPb,
        FlashOptions as FlashOptionsPb, FlashSpec as FlashSpecPb,
        SerializableMlxConfigProfile as SerializableProfilePb,
    };

    use super::*;

    // A short, observable discriminant for an `OpCode`, so conversions whose output
    // types aren't `PartialEq` can still be asserted by table. Carries the key for
    // Lock/Unlock so those rows can pin the round-tripped value.
    fn op_tag(op: &OpCode<'_>) -> String {
        match op {
            OpCode::Noop => "noop".to_string(),
            OpCode::Lock { key } => format!("lock:{key}"),
            OpCode::Unlock { key } => format!("unlock:{key}"),
            OpCode::ApplyProfile { serialized_profile } => {
                format!("apply_profile:{}", serialized_profile.is_some())
            }
            OpCode::ApplyFirmware { profile, .. } => {
                format!("apply_firmware:{}", profile.is_some())
            }
        }
    }

    // The same discriminant for a wire `MlxDeviceAction`'s command oneof.
    fn action_tag(command: &Option<fac::mlx_device_action::Command>) -> String {
        use fac::mlx_device_action::Command as C;
        match command {
            None => "none".to_string(),
            Some(C::Noop(_)) => "noop".to_string(),
            Some(C::Lock(l)) => format!("lock:{}", l.key),
            Some(C::Unlock(u)) => format!("unlock:{}", u.key),
            Some(C::ApplyProfile(p)) => {
                format!("apply_profile:{}", p.serialized_profile.is_some())
            }
            Some(C::ApplyFirmware(f)) => format!("legacy_apply_firmware:{}", f.profile.is_some()),
            Some(C::ApplyFirmwareOperation(f)) => {
                format!("apply_firmware_operation:{}", f.profile.is_some())
            }
            Some(C::UpdateFirmware(_)) => "update_firmware".to_string(),
            Some(C::ObserveFirmwareUpdate(_)) => "observe_firmware_update".to_string(),
            Some(C::Lockdown(_)) => "lockdown".to_string(),
        }
    }

    // A minimal proto profile whose every config value parses as YAML, so the
    // SerializableProfile conversion round-trips cleanly.
    fn valid_serializable_pb() -> SerializableProfilePb {
        SerializableProfilePb {
            name: "p".to_string(),
            registry_name: "r".to_string(),
            description: None,
            config: HashMap::from([("k".to_string(), "42".to_string())]),
        }
    }

    // A proto profile carrying a config value that is not valid YAML, so the
    // SerializableProfile conversion rejects it.
    fn invalid_serializable_pb() -> SerializableProfilePb {
        SerializableProfilePb {
            name: "p".to_string(),
            registry_name: "r".to_string(),
            description: None,
            config: HashMap::from([("k".to_string(), "{unterminated".to_string())]),
        }
    }

    // A complete proto firmware profile: both required specs present, so the
    // FirmwareFlasherProfile conversion succeeds.
    fn valid_firmware_pb() -> FirmwareProfilePb {
        FirmwareProfilePb {
            firmware_spec: Some(FirmwareSpecPb::default()),
            flash_spec: Some(FlashSpecPb::default()),
            flash_options: None,
        }
    }

    // -- TryFrom<Command> for DpaCommand: every oneof arm + every profile sub-path.
    #[test]
    fn command_pb_converts_to_dpa_command() {
        scenarios!(
            run = |cmd: Command| DpaCommand::try_from(cmd).map(|c| op_tag(&c.op));
            "noop" {
                Command::Noop(fac::MlxDeviceNoop {}) => Yields("noop".to_string()),
            }

            "lock carries its key" {
                Command::Lock(fac::MlxDeviceLock {
                    key: "secret".to_string(),
                }) => Yields("lock:secret".to_string()),
            }

            "lock with empty key" {
                Command::Lock(fac::MlxDeviceLock { key: String::new() }) => Yields("lock:".to_string()),
            }

            "unlock carries its key" {
                Command::Unlock(fac::MlxDeviceUnlock {
                    key: "secret".to_string(),
                }) => Yields("unlock:secret".to_string()),
            }

            "apply_profile with no profile stays None" {
                Command::ApplyProfile(fac::MlxDeviceApplyProfile {
                    serialized_profile: None,
                }) => Yields("apply_profile:false".to_string()),
            }

            "apply_profile with a valid profile" {
                Command::ApplyProfile(fac::MlxDeviceApplyProfile {
                    serialized_profile: Some(valid_serializable_pb()),
                }) => Yields("apply_profile:true".to_string()),
            }

            "apply_profile rejects unparseable config" {
                Command::ApplyProfile(fac::MlxDeviceApplyProfile {
                    serialized_profile: Some(invalid_serializable_pb()),
                }) => Fails,
            }

            "apply_firmware with no profile stays None" {
                Command::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    profile: None,
                    operation_id: "operation".to_string(),
                }) => Yields("apply_firmware:false".to_string()),
            }

            // DPA JSON has no phase identity or expected digests, so conversion
            // must reject rather than weakening the generic command contract.
            "update_firmware cannot become a legacy DPA command" {
                Command::UpdateFirmware(fac::MlxDeviceUpdateFirmware {
                    operation_id: "operation".into(),
                    phase_id: "update:1".into(),
                    profile: Some(valid_firmware_pb()),
                    ..Default::default()
                }) => FailsWith(
                    "UpdateFirmware requires the typed NIC firmware command path".to_string()
                ),
            }

            // Observation must never submit work, and no DPA command only
            // reads a receipt, so conversion must reject it.
            "observe_firmware_update cannot become a legacy DPA command" {
                Command::ObserveFirmwareUpdate(fac::MlxDeviceObserveFirmwareUpdate {
                    operation_id: "operation".into(),
                    phase_id: "update:1".into(),
                    ..Default::default()
                }) => FailsWith(
                    "ObserveFirmwareUpdate requires the typed NIC firmware command path".to_string()
                ),
            }

            // A lock status read or change without its controller state version
            // and physical identity would bypass the checks the typed path
            // performs.
            "lockdown cannot become a legacy DPA command" {
                Command::Lockdown(Default::default()) => FailsWith(
                    "Lockdown requires the typed NIC firmware command path"
                        .to_string()
                ),
            }

            "apply_firmware with a complete profile" {
                Command::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    profile: Some(valid_firmware_pb()),
                    operation_id: "operation".to_string(),
                }) => Yields("apply_firmware:true".to_string()),
            }

            "apply_firmware rejects a missing firmware_spec" {
                Command::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    operation_id: "operation".to_string(),
                    profile: Some(FirmwareProfilePb {
                        firmware_spec: None,
                        flash_spec: Some(FlashSpecPb::default()),
                        flash_options: None,
                    }),
                }) => FailsWith("missing firmware_spec".to_string()),
            }

            "apply_firmware rejects a missing flash_spec" {
                Command::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    operation_id: "operation".to_string(),
                    profile: Some(FirmwareProfilePb {
                        firmware_spec: Some(FirmwareSpecPb::default()),
                        flash_spec: None,
                        flash_options: None,
                    }),
                }) => FailsWith("missing flash_spec".to_string()),
            }
        );
    }

    // -- TryFrom<DpaDeviceCommand> for MlxDeviceAction: every OpCode arm. The
    // pci_name is threaded through untouched, so each row also pins it.
    #[test]
    fn dpa_device_command_converts_to_rpc_action() {
        scenarios!(
            run = |op: OpCode<'_>| {
                let action: fac::MlxDeviceAction = DpaDeviceCommand {
                    pci_name: "04:00.0".to_string(),
                    command: DpaCommand { op },
                }
                .try_into()?;
                Ok::<_, String>((action.pci_name, action_tag(&action.command)))
            };
            "noop" {
                OpCode::Noop => Yields(("04:00.0".to_string(), "noop".to_string())),
            }

            "lock carries its key" {
                OpCode::Lock {
                    key: "secret".to_string(),
                } => Yields(("04:00.0".to_string(), "lock:secret".to_string())),
            }

            "unlock carries its key" {
                OpCode::Unlock {
                    key: "secret".to_string(),
                } => Yields(("04:00.0".to_string(), "unlock:secret".to_string())),
            }

            "apply_profile with no profile stays None" {
                OpCode::ApplyProfile {
                    serialized_profile: None,
                } => Yields(("04:00.0".to_string(), "apply_profile:false".to_string())),
            }

            "apply_profile with a valid profile" {
                OpCode::ApplyProfile {
                    serialized_profile: Some(valid_serializable_pb().try_into().unwrap()),
                } => Yields(("04:00.0".to_string(), "apply_profile:true".to_string())),
            }

            "apply_firmware with no profile stays None" {
                OpCode::ApplyFirmware {
                    profile: None,
                    operation_id: "operation".to_string(),
                } => Yields(("04:00.0".to_string(), "apply_firmware_operation:false".to_string())),
            }

            "apply_firmware with a profile" {
                OpCode::ApplyFirmware {
                    operation_id: "operation".to_string(),
                    profile: Some(Box::new(Cow::Owned(
                        valid_firmware_pb().try_into().unwrap(),
                    ))),
                } => Yields(("04:00.0".to_string(), "apply_firmware_operation:true".to_string())),
            }

            "apply_firmware rejects an empty operation ID" {
                OpCode::ApplyFirmware {
                    profile: None,
                    operation_id: String::new(),
                } => FailsWith("firmware operation_id must not be empty".to_string()),
            }
        );
    }

    // -- TryFrom<&MlxDeviceAction> for DpaCommand: every command-oneof arm, plus the
    // None arm that also lands on Noop, and the two profile rejection paths.
    #[test]
    fn rpc_action_converts_to_dpa_command() {
        use fac::mlx_device_action::Command as C;
        scenarios!(
            run = |command: Option<fac::mlx_device_action::Command>| {
                let action = fac::MlxDeviceAction {
                    pci_name: "04:00.0".to_string(),
                    command,
                };
                DpaCommand::try_from(&action).map(|c| op_tag(&c.op))
            };
            "absent command is treated as noop" {
                None => Yields("noop".to_string()),
            }

            "noop" {
                Some(C::Noop(fac::MlxDeviceNoop {})) => Yields("noop".to_string()),
            }

            "lock carries its key" {
                Some(C::Lock(fac::MlxDeviceLock {
                    key: "secret".to_string(),
                })) => Yields("lock:secret".to_string()),
            }

            "unlock carries its key" {
                Some(C::Unlock(fac::MlxDeviceUnlock {
                    key: "secret".to_string(),
                })) => Yields("unlock:secret".to_string()),
            }

            "apply_profile with no profile stays None" {
                Some(C::ApplyProfile(fac::MlxDeviceApplyProfile {
                    serialized_profile: None,
                })) => Yields("apply_profile:false".to_string()),
            }

            "apply_profile with a valid profile" {
                Some(C::ApplyProfile(fac::MlxDeviceApplyProfile {
                    serialized_profile: Some(valid_serializable_pb()),
                })) => Yields("apply_profile:true".to_string()),
            }

            "apply_profile rejects unparseable config" {
                Some(C::ApplyProfile(fac::MlxDeviceApplyProfile {
                    serialized_profile: Some(invalid_serializable_pb()),
                })) => Fails,
            }

            "apply_firmware with no profile stays None" {
                Some(C::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    profile: None,
                    operation_id: "operation".to_string(),
                })) => Yields("apply_firmware:false".to_string()),
            }

            "apply_firmware with a complete profile" {
                Some(C::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    profile: Some(valid_firmware_pb()),
                    operation_id: "operation".to_string(),
                })) => Yields("apply_firmware:true".to_string()),
            }

            "apply_firmware rejects a missing firmware_spec" {
                Some(C::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    operation_id: "operation".to_string(),
                    profile: Some(FirmwareProfilePb {
                        firmware_spec: None,
                        flash_spec: Some(FlashSpecPb::default()),
                        flash_options: None,
                    }),
                })) => FailsWith("missing firmware_spec".to_string()),
            }
        );
    }

    /// Neither decoder accepts firmware work without durable operation identity.
    #[test]
    fn firmware_command_decoders_reject_uncorrelated_work() {
        value_scenarios!(
            run = |command: Command| {
                let action = fac::MlxDeviceAction {
                    pci_name: "04:00.0".to_string(),
                    command: Some(command.clone()),
                };
                [
                    DpaCommand::try_from(command).is_err(),
                    DpaCommand::try_from(&action).is_err(),
                ]
            };
            "legacy firmware command" {
                Command::ApplyFirmware(fac::MlxDeviceApplyFirmware {
                    profile: Some(valid_firmware_pb()),
                }) => [true, true],
            }
            "empty operation ID" {
                Command::ApplyFirmwareOperation(fac::MlxDeviceApplyFirmwareOperation {
                    profile: Some(valid_firmware_pb()),
                    operation_id: String::new(),
                }) => [true, true],
            }
        );
    }

    /// Both command decoders preserve identity so conversion cannot turn a retry
    /// into new work.
    #[test]
    fn firmware_operation_identity_survives_command_conversions() {
        let expected = fac::MlxDeviceAction {
            pci_name: "04:00.0".to_string(),
            command: Some(Command::ApplyFirmwareOperation(
                fac::MlxDeviceApplyFirmwareOperation {
                    // Omitted options are normalized by profile conversion;
                    // explicit options keep this proof about operation identity.
                    profile: Some(FirmwareProfilePb {
                        flash_options: Some(FlashOptionsPb {
                            reset_level: 3,
                            ..Default::default()
                        }),
                        ..valid_firmware_pb()
                    }),
                    operation_id: "dpa-firmware:card:state-version".to_string(),
                },
            )),
        };

        for command in [
            DpaCommand::try_from(expected.command.clone().unwrap()).unwrap(),
            DpaCommand::try_from(&expected).unwrap(),
        ] {
            let actual: fac::MlxDeviceAction = DpaDeviceCommand {
                pci_name: expected.pci_name.clone(),
                command,
            }
            .try_into()
            .unwrap();
            assert_eq!(actual, expected);
        }
    }
}
