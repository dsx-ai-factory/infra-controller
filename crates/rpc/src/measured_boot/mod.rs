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

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use measured_boot::DisplayName;

use crate::errors::RpcDataConversionError;

pub mod bundle;
pub mod journal;
pub mod machine;
pub mod pcr;
pub mod profile;
pub mod records;
pub mod report;
pub mod site;

pub trait FromGrpc<M>: TryFrom<M, Error = RpcDataConversionError> + DisplayName {
    fn from_grpc(msg: M) -> Result<Self, RpcDataConversionError> {
        Self::try_from(msg).map_err(|e| {
            RpcDataConversionError::InvalidArgument(format!(
                "bad message: {}: {e}",
                Self::display_name()
            ))
        })
    }
}

pub trait FromGrpcOpt<M>: FromGrpc<M> {
    fn from_grpc_opt(msg: Option<M>) -> Result<Self, RpcDataConversionError> {
        msg.ok_or_else(|| {
            RpcDataConversionError::InvalidArgument(format!(
                "{} is unexpectedly empty",
                Self::display_name()
            ))
        })
        .and_then(Self::from_grpc)
    }
}

pub trait FromPbVec<M: Clone>: FromGrpc<M> {
    fn from_pb_vec(pbs: &[M]) -> Result<Vec<Self>, RpcDataConversionError> {
        pbs.iter()
            .map(|record| Self::from_grpc(record.clone()))
            .collect()
    }
}

fn conv_timestamp_opt(
    ts: Option<crate::Timestamp>,
    name: &'static str,
) -> Result<DateTime<Utc>, RpcDataConversionError> {
    ts.ok_or(RpcDataConversionError::MissingArgument(name))?
        .try_into()
        .map_err(|err| RpcDataConversionError::InvalidArgument(format!("timestamp: {err}")))
}

fn conv_machine_id(machine_id: &str) -> Result<MachineId, RpcDataConversionError> {
    MachineId::from_str(machine_id)
        .map_err(|_| RpcDataConversionError::InvalidMachineId(machine_id.into()))
}
