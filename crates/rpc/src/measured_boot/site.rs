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
 *  Code for working the measurement_trusted_machines and measurement_trusted_profiles
 *  tables in the database, leveraging the site-specific record types.
 *
 * This also provides code for importing/exporting (and working with) SiteModels.
 */
use std::convert::{From, Into};
use std::vec::Vec;

use measured_boot::records::{
    MeasurementBundleRecord, MeasurementBundleValueRecord, MeasurementSystemProfileAttrRecord,
    MeasurementSystemProfileRecord,
};
use measured_boot::site::{
    ImportResult, MachineAttestationSummary, MachineAttestationSummaryList, SiteModel,
};

use crate::errors::RpcDataConversionError;
use crate::measured_boot::{FromGrpc, FromGrpcOpt, FromPbVec, conv_machine_id, conv_timestamp_opt};
use crate::protos::measured_boot::{
    ImportSiteMeasurementsResponse, ListAttestationSummaryResponse, MachineAttestationSummaryPb,
    SiteModelPb,
};

impl From<&ImportSiteMeasurementsResponse> for ImportResult {
    fn from(msg: &ImportSiteMeasurementsResponse) -> Self {
        Self {
            status: msg.result().as_str_name().to_string(),
        }
    }
}

impl FromGrpc<SiteModelPb> for SiteModel {}

impl FromGrpcOpt<SiteModelPb> for SiteModel {}

impl TryFrom<SiteModelPb> for SiteModel {
    type Error = RpcDataConversionError;

    fn try_from(model: SiteModelPb) -> Result<Self, Self::Error> {
        Ok(Self {
            measurement_system_profiles: MeasurementSystemProfileRecord::from_pb_vec(
                &model.measurement_system_profiles,
            )?,
            measurement_system_profiles_attrs: MeasurementSystemProfileAttrRecord::from_pb_vec(
                &model.measurement_system_profiles_attrs,
            )?,
            measurement_bundles: MeasurementBundleRecord::from_pb_vec(&model.measurement_bundles)?,
            measurement_bundles_values: MeasurementBundleValueRecord::from_pb_vec(
                &model.measurement_bundles_values,
            )?,
        })
    }
}

impl From<SiteModel> for SiteModelPb {
    fn from(model: SiteModel) -> Self {
        let measurement_system_profiles = model
            .measurement_system_profiles
            .into_iter()
            .map(|record| record.into())
            .collect();

        let measurement_system_profiles_attrs = model
            .measurement_system_profiles_attrs
            .into_iter()
            .map(|record| record.into())
            .collect();

        let measurement_bundles = model
            .measurement_bundles
            .into_iter()
            .map(|record| record.into())
            .collect();

        let measurement_bundles_values = model
            .measurement_bundles_values
            .into_iter()
            .map(|record| record.into())
            .collect();

        Self {
            measurement_system_profiles,
            measurement_system_profiles_attrs,
            measurement_bundles,
            measurement_bundles_values,
        }
    }
}

impl From<MachineAttestationSummaryList> for ListAttestationSummaryResponse {
    fn from(val: MachineAttestationSummaryList) -> Self {
        Self {
            attestation_outcomes: val
                .0
                .into_iter()
                .map(|e| MachineAttestationSummaryPb {
                    machine_id: e.machine_id.to_string(),
                    bundle_id: e.bundle_id,
                    profile_name: e.profile_name,
                    ts: Some(e.ts.into()),
                })
                .collect(),
        }
    }
}

impl TryFrom<ListAttestationSummaryResponse> for MachineAttestationSummaryList {
    type Error = RpcDataConversionError;

    fn try_from(val: ListAttestationSummaryResponse) -> Result<Self, Self::Error> {
        let mut attestation_summary_list = Vec::<MachineAttestationSummary>::new();

        for pb in val.attestation_outcomes {
            attestation_summary_list.push(MachineAttestationSummary {
                machine_id: conv_machine_id(&pb.machine_id)?,
                bundle_id: pb.bundle_id,
                profile_name: pb.profile_name,
                ts: pb
                    .ts
                    .map(|ts| conv_timestamp_opt(Some(ts), "ts"))
                    .transpose()?
                    .unwrap_or_default(),
            });
        }

        Ok(Self(attestation_summary_list))
    }
}
