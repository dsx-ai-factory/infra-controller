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
 *  Code for working the measuremment_journal and measurement_journal_values
 *  tables in the database, leveraging the journal-specific record types.
 */

use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::{
    MeasurementBundleId, MeasurementJournalId, MeasurementReportId, MeasurementSystemProfileId,
};
use chrono::Utc;
use serde::Serialize;
#[cfg(feature = "cli")]
use {
    crate::ToTable,
    crate::{just_print_summary, serde_just_print_summary},
};

use super::records::MeasurementMachineState;

/// MeasurementJournal is a composition of a MeasurementJournalRecord,
/// whose attributes are essentially copied directly it, as well as
/// the associated attributes (which are complete instances of
/// MeasurementReportValueRecord, along with its UUID and timestamp).
#[derive(Debug, Serialize, Clone)]
pub struct MeasurementJournal {
    pub journal_id: MeasurementJournalId,
    pub machine_id: MachineId,
    pub report_id: MeasurementReportId,
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub profile_id: Option<MeasurementSystemProfileId>,
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub bundle_id: Option<MeasurementBundleId>,
    pub state: MeasurementMachineState,
    pub ts: chrono::DateTime<Utc>,
}

impl MeasurementJournal {
    #[cfg(feature = "cli")]
    /// to_nested_prettytable converts a MeasurementJournal into a small
    /// prettytable::Table for the purpose of displaying as a nested
    /// table for a CandidateMachine (showing just the basics).
    pub fn to_nested_prettytable(&self) -> prettytable::Table {
        let mut journal_table = prettytable::Table::new();
        journal_table.add_row(prettytable::row!["report_id", self.report_id]);
        journal_table.add_row(prettytable::row![
            "profile_id",
            match self.profile_id {
                Some(profile_id) => profile_id.to_string(),
                None => "<none>".to_string(),
            }
        ]);
        journal_table.add_row(prettytable::row![
            "bundle_id",
            match self.bundle_id {
                Some(bundle_id) => bundle_id.to_string(),
                None => "<none>".to_string(),
            }
        ]);
        journal_table
    }
}

impl crate::DisplayName for MeasurementJournal {
    fn display_name() -> &'static str {
        "journal"
    }
}

// When `journal show <journal-id>` gets called, and the output format is
// the default table view, this gets used to print a pretty table.
#[cfg(feature = "cli")]
impl ToTable for MeasurementJournal {
    fn into_table(self) -> eyre::Result<String> {
        let profile_id: String = match self.profile_id {
            Some(profile_id) => profile_id.to_string(),
            None => "<none>".to_string(),
        };
        let bundle_id: String = match self.bundle_id {
            Some(bundle_id) => bundle_id.to_string(),
            None => "<none>".to_string(),
        };
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["journal_id", self.journal_id]);
        table.add_row(prettytable::row!["machine_id", self.machine_id]);
        if !just_print_summary() {
            table.add_row(prettytable::row!["report_id", self.report_id]);
            table.add_row(prettytable::row!["profile_id", profile_id]);
            table.add_row(prettytable::row!["bundle_id", bundle_id]);
        }
        table.add_row(prettytable::row!["state", self.state]);
        table.add_row(prettytable::row!["created_ts", self.ts]);
        Ok(table.to_string())
    }
}
