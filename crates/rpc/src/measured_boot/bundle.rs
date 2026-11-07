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
 *  Code for working the measurement_bundles and measurement_bundles_values
 *  tables in the database, leveraging the bundle-specific record types.
 */

use measured_boot::bundle::MeasurementBundle;
use measured_boot::records::{MeasurementBundleState, MeasurementBundleValueRecord};

use crate::errors::RpcDataConversionError;
use crate::measured_boot::{FromGrpc, FromGrpcOpt, conv_timestamp_opt};
use crate::protos::measured_boot::{MeasurementBundlePb, MeasurementBundleStatePb};

impl From<MeasurementBundle> for MeasurementBundlePb {
    fn from(val: MeasurementBundle) -> Self {
        let pb_state: MeasurementBundleStatePb = val.state.into();
        Self {
            bundle_id: Some(val.bundle_id),
            profile_id: Some(val.profile_id),
            name: val.name,
            state: pb_state.into(),
            values: val
                .values
                .iter()
                .map(|value| value.clone().into())
                .collect(),
            ts: Some(val.ts.into()),
        }
    }
}

impl FromGrpc<MeasurementBundlePb> for MeasurementBundle {}

impl FromGrpcOpt<MeasurementBundlePb> for MeasurementBundle {}

impl TryFrom<MeasurementBundlePb> for MeasurementBundle {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementBundlePb) -> Result<Self, Self::Error> {
        let state = msg.state();
        let values = msg
            .values
            .iter()
            .map(
                |attr| match MeasurementBundleValueRecord::try_from(attr.clone()) {
                    Ok(worked) => Ok(worked),
                    Err(failed) => Err(RpcDataConversionError::InvalidArgument(format!(
                        "attr conversion failed: {failed}"
                    ))),
                },
            )
            .collect::<Result<Vec<_>, _>>();

        Ok(Self {
            bundle_id: msg
                .bundle_id
                .ok_or(RpcDataConversionError::MissingArgument("bundle_id"))?,
            profile_id: msg
                .profile_id
                .ok_or(RpcDataConversionError::MissingArgument("profile_id"))?,
            name: msg.name.clone(),
            state: MeasurementBundleState::from(state),
            values: values?,
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}
