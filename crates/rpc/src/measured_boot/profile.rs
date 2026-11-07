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
 *  Code for working the measurement_system_profiles and measurement_system_profiles_attrs
 *  tables in the database, leveraging the profile-specific record types.
 */

use std::convert::{Into, TryFrom};

use measured_boot::profile::MeasurementSystemProfile;
use measured_boot::records::MeasurementSystemProfileAttrRecord;

use crate::errors::RpcDataConversionError;
use crate::measured_boot::{FromGrpc, FromGrpcOpt, conv_timestamp_opt};
use crate::protos::measured_boot::MeasurementSystemProfilePb;

impl FromGrpc<MeasurementSystemProfilePb> for MeasurementSystemProfile {}

impl FromGrpcOpt<MeasurementSystemProfilePb> for MeasurementSystemProfile {}

impl From<MeasurementSystemProfile> for MeasurementSystemProfilePb {
    fn from(val: MeasurementSystemProfile) -> Self {
        Self {
            profile_id: Some(val.profile_id),
            name: val.name,
            ts: Some(val.ts.into()),
            attrs: val.attrs.iter().map(|attr| attr.clone().into()).collect(),
        }
    }
}

impl TryFrom<MeasurementSystemProfilePb> for MeasurementSystemProfile {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementSystemProfilePb) -> Result<Self, Self::Error> {
        let attrs = msg
            .attrs
            .into_iter()
            .map(
                |attr| match MeasurementSystemProfileAttrRecord::try_from(attr) {
                    Ok(worked) => Ok(worked),
                    Err(failed) => Err(RpcDataConversionError::InvalidArgument(format!(
                        "attr conversion failed: {failed}"
                    ))),
                },
            )
            .collect::<Result<Vec<_>, _>>();

        Ok(Self {
            profile_id: msg
                .profile_id
                .ok_or(RpcDataConversionError::InvalidArgument(
                    "missing profile_id".to_string(),
                ))?,
            name: msg.name.clone(),
            attrs: attrs?,
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}
