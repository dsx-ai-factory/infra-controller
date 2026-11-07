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
 *  Code for working the measuremment_reports and measurement_reports_values
 *  tables in the database, leveraging the report-specific record types.
 */

use measured_boot::records::MeasurementReportValueRecord;
use measured_boot::report::MeasurementReport;

use crate::errors::RpcDataConversionError;
use crate::measured_boot::{FromGrpc, FromGrpcOpt, conv_machine_id, conv_timestamp_opt};
use crate::protos::measured_boot::MeasurementReportPb;

impl FromGrpc<MeasurementReportPb> for MeasurementReport {}

impl FromGrpcOpt<MeasurementReportPb> for MeasurementReport {}

impl From<MeasurementReport> for MeasurementReportPb {
    fn from(val: MeasurementReport) -> Self {
        Self {
            report_id: Some(val.report_id),
            machine_id: val.machine_id.to_string(),
            values: val
                .values
                .iter()
                .map(|value| value.clone().into())
                .collect(),
            ts: Some(val.ts.into()),
        }
    }
}

impl TryFrom<MeasurementReportPb> for MeasurementReport {
    type Error = RpcDataConversionError;

    fn try_from(msg: MeasurementReportPb) -> Result<Self, Self::Error> {
        let values = msg
            .values
            .iter()
            .map(
                |value| match MeasurementReportValueRecord::try_from(value.clone()) {
                    Ok(worked) => Ok(worked),
                    Err(failed) => Err(RpcDataConversionError::InvalidArgument(format!(
                        "attr conversion failed: {failed}"
                    ))),
                },
            )
            .collect::<Result<Vec<_>, _>>();

        Ok(Self {
            report_id: msg
                .report_id
                .ok_or(RpcDataConversionError::MissingArgument("report_id"))?,
            machine_id: conv_machine_id(&msg.machine_id)?,
            values: values?,
            ts: conv_timestamp_opt(msg.ts, "ts")?,
        })
    }
}
