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

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_libmlx_model::firmware::FirmwareSpec;
use carbide_libmlx_model::firmware::operation::{
    FirmwareOperation, FirmwareOperationPhase, FirmwareOperationState, ObserverExitBehavior,
};
use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use carbide_libmlx_model::nvconfig::DpuNvConfigProfile;
use carbide_utils::none_if_empty::NoneIfEmpty;
use mac_address::MacAddress;

use crate::forge::DpuNvConfigProfile as DpuNvConfigProfilePb;
use crate::protos::firmware_supervisor::ObserverExitBehavior as ObserverExitBehaviorPb;
use crate::protos::mlx_device::{
    FirmwareFlashReport as FirmwareFlashReportPb, FirmwareOperation as FirmwareOperationPb,
    FirmwareOperationPhase as FirmwareOperationPhasePb,
    FirmwareOperationState as FirmwareOperationStatePb, FirmwareSpec as FirmwareSpecPb,
    MlxDeviceInfo as MlxDeviceInfoPb,
};

impl From<DpuNvConfigProfile> for DpuNvConfigProfilePb {
    fn from(profile: DpuNvConfigProfile) -> Self {
        match profile {
            DpuNvConfigProfile::Gb200B3240V1 => Self::Gb200B3240V1,
        }
    }
}

// Implement conversion from Rust MlxDeviceInfo to protobuf.
impl From<MlxDeviceInfo> for MlxDeviceInfoPb {
    fn from(info: MlxDeviceInfo) -> Self {
        MlxDeviceInfoPb {
            pci_name: info.pci_name,
            device_type: info.device_type,
            psid: info.psid.unwrap_or_default(),
            device_description: info.device_description.unwrap_or_default(),
            part_number: info.part_number.unwrap_or_default(),
            fw_version_current: info.fw_version_current.unwrap_or_default(),
            pxe_version_current: info.pxe_version_current.unwrap_or_default(),
            uefi_version_current: info.uefi_version_current.unwrap_or_default(),
            uefi_version_virtio_blk_current: info
                .uefi_version_virtio_blk_current
                .unwrap_or_default(),
            uefi_version_virtio_net_current: info
                .uefi_version_virtio_net_current
                .unwrap_or_default(),
            base_mac: info.base_mac.map(|mac| mac.to_string()).unwrap_or_default(),
            base_guid: info.base_guid,
            status: info.status.unwrap_or_default(),
        }
    }
}

// Implement conversion from protobuf MlxDeviceInfo to Rust.
impl TryFrom<MlxDeviceInfoPb> for MlxDeviceInfo {
    type Error = String;

    fn try_from(proto: MlxDeviceInfoPb) -> Result<Self, Self::Error> {
        let base_mac = if proto.base_mac.is_empty() {
            None
        } else {
            Some(
                MacAddress::from_str(&proto.base_mac)
                    .map_err(|e| format!("Invalid MAC address '{}': {}", proto.base_mac, e))?,
            )
        };

        Ok(MlxDeviceInfo {
            pci_name: proto.pci_name,
            device_type: proto.device_type,
            psid: proto.psid.none_if_empty(),
            device_description: proto.device_description.none_if_empty(),
            part_number: proto.part_number.none_if_empty(),
            fw_version_current: proto.fw_version_current.none_if_empty(),
            pxe_version_current: proto.pxe_version_current.none_if_empty(),
            uefi_version_current: proto.uefi_version_current.none_if_empty(),
            uefi_version_virtio_blk_current: proto.uefi_version_virtio_blk_current.none_if_empty(),
            uefi_version_virtio_net_current: proto.uefi_version_virtio_net_current.none_if_empty(),
            status: proto.status.none_if_empty(),
            base_mac,
            base_guid: proto.base_guid,
        })
    }
}

impl From<FirmwareSpec> for FirmwareSpecPb {
    fn from(spec: FirmwareSpec) -> Self {
        FirmwareSpecPb {
            part_number: spec.part_number,
            psid: spec.psid,
            version: spec.version,
        }
    }
}

impl From<FirmwareSpecPb> for FirmwareSpec {
    fn from(proto: FirmwareSpecPb) -> Self {
        FirmwareSpec {
            part_number: proto.part_number,
            psid: proto.psid,
            version: proto.version,
        }
    }
}

// From implementations for converting FirmwareFlashReport
// to/from a FirmwareFlashReportPb protobuf message and back.
impl From<FirmwareFlashReport> for FirmwareFlashReportPb {
    fn from(result: FirmwareFlashReport) -> Self {
        FirmwareFlashReportPb {
            flashed: result.flashed,
            reset: result.reset,
            verified_image: result.verified_image,
            verified_version: result.verified_version,
            observed_version: result.observed_version,
            expected_version: result.expected_version,
        }
    }
}

impl From<FirmwareFlashReportPb> for FirmwareFlashReport {
    fn from(proto: FirmwareFlashReportPb) -> Self {
        FirmwareFlashReport {
            flashed: proto.flashed,
            reset: proto.reset,
            verified_image: proto.verified_image,
            verified_version: proto.verified_version,
            observed_version: proto.observed_version,
            expected_version: proto.expected_version,
        }
    }
}

impl From<FirmwareOperation> for FirmwareOperationPb {
    fn from(operation: FirmwareOperation) -> Self {
        let state = match operation.state {
            FirmwareOperationState::Accepted => FirmwareOperationStatePb::Accepted,
            FirmwareOperationState::Running => FirmwareOperationStatePb::Running,
            FirmwareOperationState::CancelRequested => FirmwareOperationStatePb::CancelRequested,
            FirmwareOperationState::Complete => FirmwareOperationStatePb::Complete,
            FirmwareOperationState::Canceled => FirmwareOperationStatePb::Canceled,
            FirmwareOperationState::Interrupted => FirmwareOperationStatePb::Interrupted,
            FirmwareOperationState::ManualIntervention => {
                FirmwareOperationStatePb::ManualIntervention
            }
            FirmwareOperationState::Failed => FirmwareOperationStatePb::Failed,
        };
        let phase = operation.phase.map(|phase| {
            let phase = match phase {
                FirmwareOperationPhase::Inspect => FirmwareOperationPhasePb::Inspect,
                FirmwareOperationPhase::Prepare => FirmwareOperationPhasePb::Prepare,
                FirmwareOperationPhase::Configure => FirmwareOperationPhasePb::Configure,
                FirmwareOperationPhase::Burn => FirmwareOperationPhasePb::Burn,
                FirmwareOperationPhase::Activate => FirmwareOperationPhasePb::Activate,
                FirmwareOperationPhase::VerifyImage => FirmwareOperationPhasePb::VerifyImage,
                FirmwareOperationPhase::VerifyVersion => FirmwareOperationPhasePb::VerifyVersion,
            };
            phase as i32
        });

        Self {
            operation_id: operation.operation_id,
            state: state as i32,
            phase,
            report: Some(operation.report.into()),
            diagnostic: operation.diagnostic,
        }
    }
}

impl TryFrom<FirmwareOperationPb> for FirmwareOperation {
    type Error = String;

    fn try_from(proto: FirmwareOperationPb) -> Result<Self, Self::Error> {
        if proto.operation_id.is_empty() {
            return Err("firmware operation ID is required".to_string());
        }
        let state = match FirmwareOperationStatePb::try_from(proto.state) {
            Ok(FirmwareOperationStatePb::Accepted) => FirmwareOperationState::Accepted,
            Ok(FirmwareOperationStatePb::Running) => FirmwareOperationState::Running,
            Ok(FirmwareOperationStatePb::CancelRequested) => {
                FirmwareOperationState::CancelRequested
            }
            Ok(FirmwareOperationStatePb::Complete) => FirmwareOperationState::Complete,
            Ok(FirmwareOperationStatePb::Canceled) => FirmwareOperationState::Canceled,
            Ok(FirmwareOperationStatePb::Interrupted) => FirmwareOperationState::Interrupted,
            Ok(FirmwareOperationStatePb::ManualIntervention) => {
                FirmwareOperationState::ManualIntervention
            }
            Ok(FirmwareOperationStatePb::Failed) => FirmwareOperationState::Failed,
            Ok(FirmwareOperationStatePb::Unspecified) | Err(_) => {
                return Err(format!("invalid firmware operation state {}", proto.state));
            }
        };
        let phase = proto
            .phase
            .map(|phase| match FirmwareOperationPhasePb::try_from(phase) {
                Ok(FirmwareOperationPhasePb::Inspect) => Ok(FirmwareOperationPhase::Inspect),
                Ok(FirmwareOperationPhasePb::Prepare) => Ok(FirmwareOperationPhase::Prepare),
                Ok(FirmwareOperationPhasePb::Configure) => Ok(FirmwareOperationPhase::Configure),
                Ok(FirmwareOperationPhasePb::Burn) => Ok(FirmwareOperationPhase::Burn),
                Ok(FirmwareOperationPhasePb::Activate) => Ok(FirmwareOperationPhase::Activate),
                Ok(FirmwareOperationPhasePb::VerifyImage) => {
                    Ok(FirmwareOperationPhase::VerifyImage)
                }
                Ok(FirmwareOperationPhasePb::VerifyVersion) => {
                    Ok(FirmwareOperationPhase::VerifyVersion)
                }
                Ok(FirmwareOperationPhasePb::Unspecified) | Err(_) => {
                    Err(format!("invalid firmware operation phase {phase}"))
                }
            })
            .transpose()?;

        Ok(Self {
            operation_id: proto.operation_id,
            state,
            phase,
            report: proto
                .report
                .ok_or("firmware operation report is required")?
                .into(),
            diagnostic: proto.diagnostic,
        })
    }
}

impl From<ObserverExitBehavior> for ObserverExitBehaviorPb {
    fn from(behavior: ObserverExitBehavior) -> Self {
        match behavior {
            ObserverExitBehavior::Continue => Self::Continue,
            ObserverExitBehavior::Terminate => Self::Terminate,
        }
    }
}

impl From<ObserverExitBehaviorPb> for ObserverExitBehavior {
    fn from(behavior: ObserverExitBehaviorPb) -> Self {
        match behavior {
            ObserverExitBehaviorPb::Continue => Self::Continue,
            ObserverExitBehaviorPb::Terminate => Self::Terminate,
        }
    }
}

#[cfg(test)]
mod test {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[test]
    fn test_firmware_spec_roundtrip() {
        let original = FirmwareSpec {
            part_number: "900-9D3B4-00CV-TA0".to_string(),
            psid: "MT_0000000884".to_string(),
            version: "32.43.1014".to_string(),
        };
        let proto: FirmwareSpecPb = original.clone().into();
        let converted: FirmwareSpec = proto.into();
        assert_eq!(original, converted);
    }

    // Proto -> model `TryFrom`: every input proto should convert back to the
    // expected `MlxDeviceInfo`, with empty proto strings (and an empty MAC)
    // becoming `None`. The roundtrip rows feed a model through its own
    // `Into<MlxDeviceInfoPb>` first.
    #[test]
    fn device_info_proto_to_model() {
        scenarios!(
            run = |proto: MlxDeviceInfoPb| MlxDeviceInfo::try_from(proto).map_err(drop);
            "full device roundtrips" {
                MlxDeviceInfo::create_test_device().into() => Yields(MlxDeviceInfo::create_test_device()),
            }

            "missing-data device roundtrips" {
                MlxDeviceInfo::create_test_device_with_missing_data().into() => Yields(MlxDeviceInfo::create_test_device_with_missing_data()),
            }

            "empty string fields become none" {
                MlxDeviceInfoPb {
                    pci_name: "01:00.0".to_string(),
                    device_type: "BlueField3".to_string(),
                    psid: "".to_string(), // Empty string should become None
                    device_description: "".to_string(),
                    part_number: "".to_string(),
                    fw_version_current: "".to_string(),
                    pxe_version_current: "".to_string(),
                    uefi_version_current: "".to_string(),
                    uefi_version_virtio_blk_current: "".to_string(),
                    uefi_version_virtio_net_current: "".to_string(),
                    base_mac: "".to_string(), // Empty MAC becomes None
                    base_guid: None,
                    status: "".to_string(),
                } => Yields(MlxDeviceInfo {
                    pci_name: "01:00.0".to_string(),
                    device_type: "BlueField3".to_string(),
                    psid: None,
                    device_description: None,
                    part_number: None,
                    fw_version_current: None,
                    pxe_version_current: None,
                    uefi_version_current: None,
                    uefi_version_virtio_blk_current: None,
                    uefi_version_virtio_net_current: None,
                    base_mac: None,
                    base_guid: None,
                    status: None,
                }),
            }
        );
    }

    // `FirmwareFlashReport` -> proto -> `FirmwareFlashReport` roundtrip. The
    // report isn't `PartialEq`, so project to the tuple of fields the originals
    // asserted; the conversions are total (`From`), hence `check_values`.
    #[test]
    fn firmware_flash_report_roundtrip() {
        value_scenarios!(
            run = |original: FirmwareFlashReport| {
                let proto: FirmwareFlashReportPb = original.into();
                let converted: FirmwareFlashReport = proto.into();
                (
                    converted.flashed,
                    converted.reset,
                    converted.verified_image,
                    converted.verified_version,
                    converted.observed_version,
                    converted.expected_version,
                )
            };
            "all steps success" {
                FirmwareFlashReport {
                    flashed: true,
                    reset: Some(true),
                    verified_image: Some(true),
                    verified_version: Some(true),
                    observed_version: Some("32.43.1014".to_string()),
                    expected_version: Some("32.43.1014".to_string()),
                } => (
                    true,
                    Some(true),
                    Some(true),
                    Some(true),
                    Some("32.43.1014".to_string()),
                    Some("32.43.1014".to_string()),
                ),
            }

            "flash only" {
                FirmwareFlashReport {
                    flashed: true,
                    reset: None,
                    verified_image: None,
                    verified_version: None,
                    observed_version: None,
                    expected_version: None,
                } => (true, None, None, None, None, None),
            }

            "partial failure" {
                FirmwareFlashReport {
                    flashed: true,
                    reset: Some(false),
                    verified_image: Some(false),
                    verified_version: Some(false),
                    observed_version: Some("32.42.900".to_string()),
                    expected_version: Some("32.43.1014".to_string()),
                } => (
                    true,
                    Some(false),
                    Some(false),
                    Some(false),
                    Some("32.42.900".to_string()),
                    Some("32.43.1014".to_string()),
                ),
            }
        );
    }

    /// Preserve exact execution states and phases without interpreting partial
    /// progress as successful completion. Each enum value is paired once.
    #[test]
    fn firmware_operation_roundtrip_preserves_execution_state() {
        use FirmwareOperationPhase as Phase;
        use FirmwareOperationState as State;

        value_scenarios!(
            run = |(state, phase): (State, Option<Phase>)| {
                // A reported Complete must not erase a failed requested reset;
                // completion checks belong to the operation's owner.
                let original = FirmwareOperation {
                    operation_id: "svpc:card-1:V2@2026-10-21T20:00:00Z".to_string(),
                    state,
                    phase,
                    report: FirmwareFlashReport {
                        flashed: true,
                        reset: Some(false),
                        verified_image: None,
                        verified_version: Some(false),
                        observed_version: Some("32.42.900".to_string()),
                        expected_version: Some("32.43.1014".to_string()),
                    },
                    diagnostic: phase.map(|_| "reset not confirmed".to_string()),
                };
                let proto = FirmwareOperationPb::from(original.clone());
                let converted = FirmwareOperation::try_from(proto.clone())
                    .expect("convert firmware operation");

                assert_eq!(converted.operation_id, original.operation_id);
                assert_eq!(converted.state, state);
                assert_eq!(converted.phase, phase);
                assert_eq!(converted.diagnostic, original.diagnostic);
                assert!(!converted.is_complete());
                assert_eq!(
                    FirmwareFlashReportPb::from(converted.report),
                    FirmwareFlashReportPb::from(original.report),
                );
                (proto.state, proto.phase)
            };
            "state and phase wire identities" {
                // Absence is distinct from a present UNSPECIFIED phase.
                (State::Accepted, None) => (1, None),
                (State::Running, Some(Phase::Inspect)) => (2, Some(1)),
                (State::CancelRequested, Some(Phase::Prepare)) => (3, Some(2)),
                (State::Complete, Some(Phase::Configure)) => (4, Some(3)),
                (State::Canceled, Some(Phase::Burn)) => (5, Some(4)),
                (State::Interrupted, Some(Phase::Activate)) => (6, Some(5)),
                (State::ManualIntervention, Some(Phase::VerifyImage)) => (7, Some(6)),
                (State::Failed, Some(Phase::VerifyVersion)) => (8, Some(7)),
            }
        );
    }

    /// Missing identity/progress and unknown execution states cannot become a
    /// default operation that a receiver might mistake for accepted work.
    #[test]
    fn firmware_operation_rejects_incomplete_or_unknown_wire_fields() {
        let valid = FirmwareOperationPb {
            operation_id: "opaque operation identifier".to_string(),
            state: FirmwareOperationStatePb::Accepted as i32,
            phase: None,
            report: Some(FirmwareFlashReportPb::default()),
            diagnostic: None,
        };
        scenarios!(
            run = |proto| FirmwareOperation::try_from(proto).map(|_| ());
            "required identity" {
                FirmwareOperationPb { operation_id: String::new(), ..valid.clone() }
                    => FailsWith("firmware operation ID is required".to_string()),
            }
            "required progress" {
                FirmwareOperationPb { report: None, ..valid.clone() }
                    => FailsWith("firmware operation report is required".to_string()),
            }
            "state must be known and specified" {
                // Zero is a known protobuf enum value, but not an execution state.
                FirmwareOperationPb { state: 0, ..valid.clone() }
                    => FailsWith("invalid firmware operation state 0".to_string()),
                // A future or corrupt numeric value must not silently default.
                FirmwareOperationPb { state: 99, ..valid.clone() }
                    => FailsWith("invalid firmware operation state 99".to_string()),
            }
            "present phase must be known and specified" {
                FirmwareOperationPb { phase: Some(0), ..valid.clone() }
                    => FailsWith("invalid firmware operation phase 0".to_string()),
                FirmwareOperationPb { phase: Some(99), ..valid }
                    => FailsWith("invalid firmware operation phase 99".to_string()),
            }
        );
    }

    /// The private RPC preserves the explicit observer-exit policy; transport
    /// disconnection does not select either behavior by itself.
    #[test]
    fn observer_exit_behavior_roundtrip_preserves_policy() {
        value_scenarios!(
            run = |behavior| {
                let proto = ObserverExitBehaviorPb::from(behavior);
                assert_eq!(ObserverExitBehavior::from(proto), behavior);
                proto as i32
            };
            "observer process exit" {
                ObserverExitBehavior::Continue => 0,
                ObserverExitBehavior::Terminate => 1,
            }
        );
    }

    #[test]
    fn test_flasher_result_default() {
        let report = FirmwareFlashReport::default();
        assert!(!report.flashed);
        assert!(report.reset.is_none());
        assert!(report.verified_image.is_none());
        assert!(report.verified_version.is_none());
        assert!(report.observed_version.is_none());
        assert!(report.expected_version.is_none());
    }
}
