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

use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementReportId;
use chrono::Utc;
use serde::Serialize;

use super::pcr::PcrRegisterValue;
use super::records::MeasurementReportValueRecord;
#[cfg(feature = "cli")]
use crate::ToTable;

/// MeasurementReport is a composition of a MeasurementReportRecord,
/// whose attributes are essentially copied directly it, as well as
/// the associated attributes (which are complete instances of
/// MeasurementReportValueRecord, along with its UUID and timestamp).
#[derive(Debug, Serialize, Clone)]
pub struct MeasurementReport {
    pub report_id: MeasurementReportId,
    pub machine_id: MachineId,
    pub ts: chrono::DateTime<Utc>,
    pub values: Vec<MeasurementReportValueRecord>,
}

impl MeasurementReport {
    pub fn pcr_values(&self) -> Vec<PcrRegisterValue> {
        let borrowed = &self.values;
        borrowed.iter().map(|rec| rec.clone().into()).collect()
    }
}

impl crate::DisplayName for MeasurementReport {
    fn display_name() -> &'static str {
        "report"
    }
}

// When `report show <report-id>` gets called, and the output format is
// the default table view, this gets used to print a pretty table.
#[cfg(feature = "cli")]
impl ToTable for MeasurementReport {
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
        table.add_row(prettytable::row!["report_id", self.report_id]);
        table.add_row(prettytable::row!["machine_id", self.machine_id]);
        table.add_row(prettytable::row!["created_ts", self.ts]);
        table.add_row(prettytable::row!["values", values_table]);
        Ok(table.to_string())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use carbide_uuid::measured_boot::MeasurementReportValueId;
    use chrono::DateTime;

    use super::*;

    fn report_value(pcr_register: i16, sha_any: &str) -> MeasurementReportValueRecord {
        MeasurementReportValueRecord {
            value_id: MeasurementReportValueId::new(),
            report_id: MeasurementReportId::new(),
            pcr_register,
            sha_any: sha_any.to_string(),
            ts: DateTime::<Utc>::UNIX_EPOCH,
        }
    }

    #[test]
    fn pcr_value_projection_cases() {
        check_values(
            [
                Check {
                    scenario: "empty report",
                    input: vec![],
                    expect: vec![],
                },
                Check {
                    scenario: "populated report preserves record order",
                    input: vec![report_value(2, "sha-2"), report_value(11, "sha-11")],
                    expect: vec![
                        PcrRegisterValue {
                            pcr_register: 2,
                            sha_any: "sha-2".to_string(),
                        },
                        PcrRegisterValue {
                            pcr_register: 11,
                            sha_any: "sha-11".to_string(),
                        },
                    ],
                },
            ],
            |values| {
                MeasurementReport {
                    report_id: MeasurementReportId::new(),
                    machine_id: MachineId::new(MachineIdSource::Tpm, [0x11; 32], MachineType::Host),
                    ts: DateTime::<Utc>::UNIX_EPOCH,
                    values,
                }
                .pcr_values()
            },
        );
    }
}
