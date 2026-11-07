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

/*!
 *  gRPC conversions between measured-boot record/enum types and their
 *  protobuf counterparts. Implements `From`/`TryFrom` along with the
 *  `FromGrpc`, `FromGrpcOpt`, and `FromPbVec` traits for the record
 *  types defined in `measured_boot::records`.
 */

use std::convert::Into;
use std::str::FromStr;

use carbide_uuid::measured_boot::TrustedMachineId;
use measured_boot::records::{
    CandidateMachineSummary, MeasurementApprovedMachineRecord, MeasurementApprovedProfileRecord,
    MeasurementApprovedType, MeasurementBundleRecord, MeasurementBundleState,
    MeasurementBundleValueRecord, MeasurementJournalRecord, MeasurementMachineState,
    MeasurementReportRecord, MeasurementReportValueRecord, MeasurementSystemProfileAttrRecord,
    MeasurementSystemProfileRecord,
};

use crate::errors::RpcDataConversionError;
use crate::measured_boot::{FromGrpc, FromGrpcOpt, FromPbVec, conv_machine_id, conv_timestamp_opt};
use crate::protos::measured_boot::{
    CandidateMachineSummaryPb, MeasurementApprovedMachineRecordPb,
    MeasurementApprovedProfileRecordPb, MeasurementApprovedTypePb, MeasurementBundleRecordPb,
    MeasurementBundleStatePb, MeasurementBundleValueRecordPb, MeasurementJournalRecordPb,
    MeasurementMachineStatePb, MeasurementReportRecordPb, MeasurementReportValueRecordPb,
    MeasurementSystemProfileAttrRecordPb, MeasurementSystemProfileRecordPb,
};

impl FromGrpc<MeasurementSystemProfileRecordPb> for MeasurementSystemProfileRecord {}

impl FromPbVec<MeasurementSystemProfileRecordPb> for MeasurementSystemProfileRecord {}

impl From<MeasurementSystemProfileRecord> for MeasurementSystemProfileRecordPb {
    fn from(val: MeasurementSystemProfileRecord) -> Self {
        Self {
            profile_id: Some(val.profile_id),
            name: val.name,
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementSystemProfileRecordPb> for MeasurementSystemProfileRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementSystemProfileRecordPb) -> Result<Self, Self::Error> {
        Ok(Self {
            profile_id: msg
                .profile_id
                .ok_or(RpcDataConversionError::MissingArgument("profile_id"))?,
            name: msg.name.clone(),
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl FromGrpc<MeasurementSystemProfileAttrRecordPb> for MeasurementSystemProfileAttrRecord {}

impl FromPbVec<MeasurementSystemProfileAttrRecordPb> for MeasurementSystemProfileAttrRecord {}

impl From<MeasurementSystemProfileAttrRecord> for MeasurementSystemProfileAttrRecordPb {
    fn from(val: MeasurementSystemProfileAttrRecord) -> Self {
        Self {
            attribute_id: Some(val.attribute_id),
            profile_id: Some(val.profile_id),
            key: val.key,
            value: val.value,
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementSystemProfileAttrRecordPb> for MeasurementSystemProfileAttrRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementSystemProfileAttrRecordPb) -> Result<Self, Self::Error> {
        Ok(Self {
            attribute_id: msg
                .attribute_id
                .ok_or(RpcDataConversionError::MissingArgument("attribute_id"))?,
            profile_id: msg
                .profile_id
                .ok_or(RpcDataConversionError::MissingArgument("profile_id"))?,
            key: msg.key.clone(),
            value: msg.value.clone(),
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl From<MeasurementBundleState> for MeasurementBundleStatePb {
    fn from(val: MeasurementBundleState) -> Self {
        match val {
            MeasurementBundleState::Pending => Self::Pending,
            MeasurementBundleState::Active => Self::Active,
            MeasurementBundleState::Obsolete => Self::Obsolete,
            MeasurementBundleState::Retired => Self::Retired,
            MeasurementBundleState::Revoked => Self::Revoked,
        }
    }
}

impl From<MeasurementBundleStatePb> for MeasurementBundleState {
    fn from(msg: MeasurementBundleStatePb) -> Self {
        match msg {
            MeasurementBundleStatePb::Pending => Self::Pending,
            MeasurementBundleStatePb::Active => Self::Active,
            MeasurementBundleStatePb::Obsolete => Self::Obsolete,
            MeasurementBundleStatePb::Retired => Self::Retired,
            MeasurementBundleStatePb::Revoked => Self::Revoked,
        }
    }
}

impl FromGrpc<MeasurementBundleRecordPb> for MeasurementBundleRecord {}

impl FromPbVec<MeasurementBundleRecordPb> for MeasurementBundleRecord {}

impl From<MeasurementBundleRecord> for MeasurementBundleRecordPb {
    fn from(val: MeasurementBundleRecord) -> Self {
        let pb_state: MeasurementBundleStatePb = val.state.into();
        Self {
            bundle_id: Some(val.bundle_id),
            name: val.name,
            profile_id: Some(val.profile_id),
            state: pb_state.into(),
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementBundleRecordPb> for MeasurementBundleRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementBundleRecordPb) -> Result<Self, Self::Error> {
        let state = msg.state();

        Ok(Self {
            bundle_id: msg
                .bundle_id
                .ok_or(RpcDataConversionError::MissingArgument("bundle_id"))?,
            profile_id: msg
                .profile_id
                .ok_or(RpcDataConversionError::MissingArgument("profile_id"))?,
            name: msg.name.clone(),
            state: MeasurementBundleState::from(state),
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl FromGrpc<MeasurementBundleValueRecordPb> for MeasurementBundleValueRecord {}

impl FromPbVec<MeasurementBundleValueRecordPb> for MeasurementBundleValueRecord {}

impl From<MeasurementBundleValueRecord> for MeasurementBundleValueRecordPb {
    fn from(val: MeasurementBundleValueRecord) -> Self {
        Self {
            value_id: Some(val.value_id),
            bundle_id: Some(val.bundle_id),
            pcr_register: val.pcr_register as i32,
            sha_any: val.sha_any,
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementBundleValueRecordPb> for MeasurementBundleValueRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementBundleValueRecordPb) -> Result<Self, Self::Error> {
        Ok(Self {
            value_id: msg
                .value_id
                .ok_or(RpcDataConversionError::MissingArgument("value_id"))?,
            bundle_id: msg
                .bundle_id
                .ok_or(RpcDataConversionError::MissingArgument("bundle_id"))?,
            pcr_register: msg.pcr_register as i16,
            sha_any: msg.sha_any.clone(),
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl FromGrpc<MeasurementReportRecordPb> for MeasurementReportRecord {}

impl From<MeasurementReportRecord> for MeasurementReportRecordPb {
    fn from(val: MeasurementReportRecord) -> Self {
        Self {
            report_id: Some(val.report_id),
            machine_id: val.machine_id.to_string(),
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementReportRecordPb> for MeasurementReportRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementReportRecordPb) -> Result<Self, Self::Error> {
        Ok(Self {
            report_id: msg
                .report_id
                .ok_or(RpcDataConversionError::MissingArgument("report_id"))?,
            machine_id: conv_machine_id(&msg.machine_id)?,
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl FromGrpc<MeasurementReportValueRecordPb> for MeasurementReportValueRecord {}

impl From<MeasurementReportValueRecord> for MeasurementReportValueRecordPb {
    fn from(val: MeasurementReportValueRecord) -> Self {
        Self {
            value_id: Some(val.value_id),
            report_id: Some(val.report_id),
            pcr_register: val.pcr_register as i32,
            sha_any: val.sha_any,
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementReportValueRecordPb> for MeasurementReportValueRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementReportValueRecordPb) -> Result<Self, Self::Error> {
        Ok(Self {
            value_id: msg
                .value_id
                .ok_or(RpcDataConversionError::MissingArgument("value_id"))?,
            report_id: msg
                .report_id
                .ok_or(RpcDataConversionError::MissingArgument("report_id"))?,
            pcr_register: msg.pcr_register as i16,
            sha_any: msg.sha_any.clone(),
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl FromGrpc<MeasurementJournalRecordPb> for MeasurementJournalRecord {}

impl From<MeasurementJournalRecord> for MeasurementJournalRecordPb {
    fn from(val: MeasurementJournalRecord) -> Self {
        let pb_state: MeasurementMachineStatePb = val.state.into();

        Self {
            journal_id: Some(val.journal_id),
            machine_id: val.machine_id.to_string(),
            report_id: Some(val.report_id),
            profile_id: val.profile_id,
            bundle_id: val.bundle_id,
            state: pb_state.into(),
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementJournalRecordPb> for MeasurementJournalRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementJournalRecordPb) -> Result<Self, Self::Error> {
        let state = msg.state();

        Ok(Self {
            journal_id: msg
                .journal_id
                .ok_or(RpcDataConversionError::MissingArgument("journal_id"))?,
            machine_id: conv_machine_id(&msg.machine_id)?,
            report_id: msg
                .report_id
                .ok_or(RpcDataConversionError::MissingArgument("report_id"))?,
            profile_id: msg.profile_id,
            bundle_id: msg.bundle_id,
            state: MeasurementMachineState::from(state),
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl From<MeasurementMachineState> for MeasurementMachineStatePb {
    fn from(val: MeasurementMachineState) -> Self {
        match val {
            MeasurementMachineState::Discovered => Self::Discovered,
            MeasurementMachineState::PendingBundle => Self::PendingBundle,
            MeasurementMachineState::Measured => Self::Measured,
            MeasurementMachineState::MeasuringFailed => Self::MeasuringFailed,
        }
    }
}

impl From<MeasurementMachineStatePb> for MeasurementMachineState {
    fn from(msg: MeasurementMachineStatePb) -> Self {
        match msg {
            MeasurementMachineStatePb::Discovered => Self::Discovered,
            MeasurementMachineStatePb::PendingBundle => Self::PendingBundle,
            MeasurementMachineStatePb::Measured => Self::Measured,
            MeasurementMachineStatePb::MeasuringFailed => Self::MeasuringFailed,
        }
    }
}

impl FromGrpc<CandidateMachineSummaryPb> for CandidateMachineSummary {}

impl From<CandidateMachineSummary> for CandidateMachineSummaryPb {
    fn from(val: CandidateMachineSummary) -> Self {
        Self {
            machine_id: val.machine_id.to_string(),
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<CandidateMachineSummaryPb> for CandidateMachineSummary {
    type Error = RpcDataConversionError;

    fn try_from(msg: CandidateMachineSummaryPb) -> Result<Self, Self::Error> {
        Ok(Self {
            machine_id: conv_machine_id(&msg.machine_id)?,
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl From<MeasurementApprovedType> for MeasurementApprovedTypePb {
    fn from(val: MeasurementApprovedType) -> Self {
        match val {
            MeasurementApprovedType::Oneshot => Self::Oneshot,
            MeasurementApprovedType::Persist => Self::Persist,
        }
    }
}

impl From<MeasurementApprovedTypePb> for MeasurementApprovedType {
    fn from(val: MeasurementApprovedTypePb) -> Self {
        match val {
            MeasurementApprovedTypePb::Oneshot => Self::Oneshot,
            MeasurementApprovedTypePb::Persist => Self::Persist,
        }
    }
}

impl FromGrpc<MeasurementApprovedMachineRecordPb> for MeasurementApprovedMachineRecord {}

impl FromGrpcOpt<MeasurementApprovedMachineRecordPb> for MeasurementApprovedMachineRecord {}

impl From<MeasurementApprovedMachineRecord> for MeasurementApprovedMachineRecordPb {
    fn from(val: MeasurementApprovedMachineRecord) -> Self {
        let approval_type: MeasurementApprovedTypePb = val.approval_type.into();

        Self {
            approval_id: Some(val.approval_id),
            machine_id: val.machine_id.to_string(),
            approval_type: approval_type.into(),
            pcr_registers: val.pcr_registers.unwrap_or("".to_string()),
            comments: val.comments.unwrap_or("".to_string()),
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementApprovedMachineRecordPb> for MeasurementApprovedMachineRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementApprovedMachineRecordPb) -> Result<Self, Self::Error> {
        let approval_type = msg.approval_type();

        Ok(Self {
            approval_id: msg
                .approval_id
                .ok_or(RpcDataConversionError::MissingArgument("approval_id"))?,
            machine_id: TrustedMachineId::from_str(&msg.machine_id).map_err(|err| {
                RpcDataConversionError::InvalidArgument(format!("trusted machine id: {err}"))
            })?,
            approval_type: MeasurementApprovedType::from(approval_type),
            pcr_registers: match !msg.pcr_registers.is_empty() {
                true => Some(msg.pcr_registers.clone()),
                false => None,
            },
            comments: match !msg.comments.is_empty() {
                true => Some(msg.comments.clone()),
                false => None,
            },
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}

impl FromGrpc<MeasurementApprovedProfileRecordPb> for MeasurementApprovedProfileRecord {}

impl FromGrpcOpt<MeasurementApprovedProfileRecordPb> for MeasurementApprovedProfileRecord {}

impl From<MeasurementApprovedProfileRecord> for MeasurementApprovedProfileRecordPb {
    fn from(val: MeasurementApprovedProfileRecord) -> Self {
        let approval_type: MeasurementApprovedTypePb = val.approval_type.into();

        Self {
            approval_id: Some(val.approval_id),
            profile_id: Some(val.profile_id),
            approval_type: approval_type.into(),
            pcr_registers: val.pcr_registers.unwrap_or("".to_string()),
            comments: val.comments.unwrap_or("".to_string()),
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementApprovedProfileRecordPb> for MeasurementApprovedProfileRecord {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementApprovedProfileRecordPb) -> Result<Self, Self::Error> {
        let approval_type = msg.approval_type();
        Ok(Self {
            approval_id: msg
                .approval_id
                .ok_or(RpcDataConversionError::MissingArgument("approval_id"))?,
            profile_id: msg
                .profile_id
                .ok_or(RpcDataConversionError::MissingArgument("profile_id"))?,
            approval_type: MeasurementApprovedType::from(approval_type),
            pcr_registers: match !msg.pcr_registers.is_empty() {
                true => Some(msg.pcr_registers.clone()),
                false => None,
            },
            comments: match !msg.comments.is_empty() {
                true => Some(msg.comments.clone()),
                false => None,
            },
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}
