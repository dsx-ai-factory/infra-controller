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
 *  Code for working the machine_topologies table in the
 *  database to match candidate machines to profiles and bundles.
 */

use measured_boot::journal::MeasurementJournal;
use measured_boot::machine::CandidateMachine;
use measured_boot::records::MeasurementMachineState;

use crate::errors::RpcDataConversionError;
use crate::measured_boot::{FromGrpc, FromGrpcOpt, conv_machine_id, conv_timestamp_opt};
use crate::protos::measured_boot::{CandidateMachinePb, MeasurementMachineStatePb};

impl FromGrpc<CandidateMachinePb> for CandidateMachine {}

impl FromGrpcOpt<CandidateMachinePb> for CandidateMachine {}

impl From<CandidateMachine> for CandidateMachinePb {
    fn from(val: CandidateMachine) -> Self {
        let pb_state: MeasurementMachineStatePb = val.state.into();
        Self {
            machine_id: val.machine_id.to_string(),
            state: pb_state.into(),
            journal: val.journal.map(|journal| journal.into()),
            attrs: val.attrs,
            created_ts: Some(val.created_ts.into()),
            updated_ts: Some(val.updated_ts.into()),
        }
    }
}

impl TryFrom<CandidateMachinePb> for CandidateMachine {
    type Error = RpcDataConversionError;

    fn try_from(msg: CandidateMachinePb) -> Result<Self, Self::Error> {
        let state = msg.state();

        Ok(Self {
            machine_id: conv_machine_id(&msg.machine_id)?,
            state: MeasurementMachineState::from(state),
            journal: match msg.journal {
                Some(journal_pb) => Some(MeasurementJournal::try_from(journal_pb)?),
                None => None,
            },
            attrs: msg.attrs,
            created_ts: conv_timestamp_opt(msg.created_ts, "created_ts")?,
            updated_ts: conv_timestamp_opt(msg.updated_ts, "updated_ts")?,
        })
    }
}
