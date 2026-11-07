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
 *  Code for working the measuremment_journal and measurement_journal_values
 *  tables in the database, leveraging the journal-specific record types.
 */

use measured_boot::journal::MeasurementJournal;
use measured_boot::records::MeasurementMachineState;

use crate::errors::RpcDataConversionError;
use crate::measured_boot::{FromGrpc, FromGrpcOpt, conv_machine_id, conv_timestamp_opt};
use crate::protos::measured_boot::{MeasurementJournalPb, MeasurementMachineStatePb};

impl FromGrpc<MeasurementJournalPb> for MeasurementJournal {}

impl FromGrpcOpt<MeasurementJournalPb> for MeasurementJournal {}

impl From<MeasurementJournal> for MeasurementJournalPb {
    fn from(val: MeasurementJournal) -> Self {
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

impl TryFrom<MeasurementJournalPb> for MeasurementJournal {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementJournalPb) -> Result<Self, Self::Error> {
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
