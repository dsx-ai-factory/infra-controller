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

use carbide_uuid::measured_boot::MeasurementSystemProfileId;
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::records::MeasurementSystemProfileAttrRecord;
#[cfg(feature = "cli")]
use crate::ToTable;

/// MeasurementSystemProfile is a composition of a MeasurementSystemProfileRecord,
/// whose attributes are essentially copied directly it, as well as
/// the associated attributes (which are complete instances of
/// MeasurementSystemProfileAttrRecord, along with its UUID and timestamp).
///
/// Included are ToTable implementations, which are used by the CLI for
/// doing prettytable-formatted output.
#[derive(Debug, Serialize)]
pub struct MeasurementSystemProfile {
    pub profile_id: MeasurementSystemProfileId,
    pub name: String,
    pub ts: DateTime<Utc>,
    pub attrs: Vec<MeasurementSystemProfileAttrRecord>,
}

impl crate::DisplayName for MeasurementSystemProfile {
    fn display_name() -> &'static str {
        "profile"
    }
}

// When `profile show <profile-id>` gets called, and the output format is
// the default table view, this gets used to print a pretty table.
#[cfg(feature = "cli")]
impl ToTable for MeasurementSystemProfile {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        let mut attrs_table = prettytable::Table::new();
        attrs_table.add_row(prettytable::row!["name", "value"]);
        for attr_record in self.attrs.iter() {
            attrs_table.add_row(prettytable::row![attr_record.key, attr_record.value]);
        }
        table.add_row(prettytable::row!["profile_id", self.profile_id]);
        table.add_row(prettytable::row!["name", self.name]);
        table.add_row(prettytable::row!["created_ts", self.ts]);
        table.add_row(prettytable::row!["attrs", attrs_table]);
        Ok(table.to_string())
    }
}
