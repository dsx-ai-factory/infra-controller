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

use std::vec::Vec;

use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementBundleId;
use chrono::Utc;
use serde::{Deserialize, Serialize};
#[cfg(feature = "sqlx")]
use sqlx::FromRow;

use super::records::{
    MeasurementBundleRecord, MeasurementBundleValueRecord, MeasurementSystemProfileAttrRecord,
    MeasurementSystemProfileRecord,
};
#[cfg(feature = "cli")]
use crate::ToTable;

#[derive(Serialize)]
pub struct ImportResult {
    pub status: String,
}

#[cfg(feature = "cli")]
impl ToTable for ImportResult {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["status", self.status]);
        Ok(table.to_string())
    }
}

/// SiteModel represents everything that is imported/exported
/// for an entire site.
#[derive(Serialize, Deserialize)]
pub struct SiteModel {
    pub measurement_system_profiles: Vec<MeasurementSystemProfileRecord>,
    pub measurement_system_profiles_attrs: Vec<MeasurementSystemProfileAttrRecord>,
    pub measurement_bundles: Vec<MeasurementBundleRecord>,
    pub measurement_bundles_values: Vec<MeasurementBundleValueRecord>,
}

#[cfg(feature = "cli")]
impl ToTable for SiteModel {
    fn into_table(self) -> eyre::Result<String> {
        Ok("lol, not implemented for SiteModel. try -o json or -o yaml.".to_string())
    }
}

impl crate::DisplayName for SiteModel {
    fn display_name() -> &'static str {
        "model"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MachineAttestationSummary {
    pub machine_id: MachineId,
    pub bundle_id: Option<MeasurementBundleId>,
    #[cfg_attr(feature = "sqlx", sqlx(rename = "name"))]
    pub profile_name: String,
    pub ts: chrono::DateTime<Utc>,
}

pub struct MachineAttestationSummaryList(pub Vec<MachineAttestationSummary>);
