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
 *  Code for working the machine_topologies table in the
 *  database to match candidate machines to profiles and bundles.
 */

use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use chrono::Utc;
use serde::Serialize;

use super::journal::MeasurementJournal;
use super::records::MeasurementMachineState;
#[cfg(feature = "cli")]
use crate::ToTable;

/// CandidateMachine describes a machine that is a candidate for attestation,
/// and is derived from machine information in the machine_toplogies table.
#[derive(Debug, Serialize, Clone)]
pub struct CandidateMachine {
    pub machine_id: MachineId,
    pub state: MeasurementMachineState,
    pub journal: Option<MeasurementJournal>,
    pub attrs: HashMap<String, String>,
    pub created_ts: chrono::DateTime<Utc>,
    pub updated_ts: chrono::DateTime<Utc>,
}

impl crate::DisplayName for CandidateMachine {
    fn display_name() -> &'static str {
        "machine"
    }
}

#[cfg(feature = "cli")]
impl ToTable for CandidateMachine {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        let journal_table = match &self.journal {
            Some(journal) => journal.to_nested_prettytable(),
            None => {
                let mut not_found = prettytable::Table::new();
                not_found.add_row(prettytable::row!["<no journal found>"]);
                not_found
            }
        };
        let mut attrs_table = prettytable::Table::new();
        attrs_table.add_row(prettytable::row!["name", "value"]);
        for (key, value) in self.attrs.iter() {
            attrs_table.add_row(prettytable::row![key, value]);
        }
        table.add_row(prettytable::row!["machine_id", self.machine_id]);
        table.add_row(prettytable::row!["state", self.state]);
        table.add_row(prettytable::row!["created_ts", self.created_ts]);
        table.add_row(prettytable::row!["updated_ts", self.updated_ts]);
        table.add_row(prettytable::row!["journal", journal_table]);
        table.add_row(prettytable::row!["attrs", attrs_table]);
        Ok(table.to_string())
    }
}
