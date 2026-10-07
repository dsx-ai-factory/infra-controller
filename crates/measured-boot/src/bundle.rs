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

use carbide_uuid::measured_boot::{MeasurementBundleId, MeasurementSystemProfileId};
use serde::Serialize;

use super::pcr::PcrRegisterValue;
use super::records::{MeasurementBundleState, MeasurementBundleValueRecord};
#[cfg(feature = "cli")]
use crate::ToTable;

/// MeasurementBundle is a composition of a MeasurementBundleRecord,
/// whose attributes are essentially copied directly it, as well as
/// the associated attributes (which are complete instances of
/// MeasurementBundleValueRecord, along with its UUID and timestamp).
#[derive(Debug, Serialize, Clone)]
pub struct MeasurementBundle {
    // bundle_id is the auto-generated UUID for a measurement bundle,
    // and is used as a reference ID for all measurement_bundle_value
    // records.
    pub bundle_id: MeasurementBundleId,

    // profile_id is the system profile this bundle is associated
    // with, allowing us to track bundles per profile.
    pub profile_id: MeasurementSystemProfileId,

    // name is the [db-enforced] unique, human-friendly name for the
    // bundle. for manually-created bundles, it is expected that
    // a name is provided. for auto-created bundles, some sort of
    // derived name is generated.
    pub name: String,

    // state is the state of this bundle.
    // See the MeasurementBundleState enum for more info,
    // including all states, and what they mean.
    pub state: MeasurementBundleState,

    // values are all of the bundle measurement values,
    // which includes all of the PCR registers and their
    // values.
    pub values: Vec<MeasurementBundleValueRecord>,

    // ts is the timestamp this bundle was created.
    pub ts: chrono::DateTime<chrono::Utc>,
}

impl MeasurementBundle {
    pub fn pcr_values(&self) -> Vec<PcrRegisterValue> {
        let borrowed = &self.values;
        borrowed.iter().map(|rec| rec.clone().into()).collect()
    }
}

impl crate::DisplayName for MeasurementBundle {
    fn display_name() -> &'static str {
        "bundle"
    }
}

// When `bundle show <bundle-id>` gets called, and the output format is
// the default table view, this gets used to print a pretty table.
#[cfg(feature = "cli")]
impl ToTable for MeasurementBundle {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        let mut values_table = prettytable::Table::new();
        values_table.add_row(prettytable::row!["pcr_register", "value"]);
        for value_record in self.values.iter() {
            values_table.add_row(prettytable::row![
                value_record.pcr_register,
                value_record.sha_any
            ]);
        }
        table.add_row(prettytable::row!["bundle_id", self.bundle_id]);
        table.add_row(prettytable::row!["profile_id", self.profile_id]);
        table.add_row(prettytable::row!["name", self.name]);
        table.add_row(prettytable::row!["state", self.state]);
        table.add_row(prettytable::row!["created_ts", self.ts]);
        table.add_row(prettytable::row!["values", values_table]);
        Ok(table.to_string())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use chrono::{DateTime, Utc};

    use super::*;

    fn bundle_value(pcr_register: i16, sha_any: &str) -> MeasurementBundleValueRecord {
        MeasurementBundleValueRecord {
            pcr_register,
            sha_any: sha_any.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn pcr_value_projection_cases() {
        check_values(
            [
                Check {
                    scenario: "empty bundle",
                    input: vec![],
                    expect: vec![],
                },
                Check {
                    scenario: "populated bundle preserves record order",
                    input: vec![bundle_value(0, "sha-0"), bundle_value(7, "sha-7")],
                    expect: vec![
                        PcrRegisterValue {
                            pcr_register: 0,
                            sha_any: "sha-0".to_string(),
                        },
                        PcrRegisterValue {
                            pcr_register: 7,
                            sha_any: "sha-7".to_string(),
                        },
                    ],
                },
            ],
            |values| {
                MeasurementBundle {
                    bundle_id: MeasurementBundleId::new(),
                    profile_id: MeasurementSystemProfileId::new(),
                    name: "test bundle".to_string(),
                    state: MeasurementBundleState::Active,
                    values,
                    ts: DateTime::<Utc>::UNIX_EPOCH,
                }
                .pcr_values()
            },
        );
    }
}
