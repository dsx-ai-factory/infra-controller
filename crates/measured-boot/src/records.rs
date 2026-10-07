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
 *  Contains structs that map directly to tables, with the intent of binding
 *  for doing selects.
 *
 *  And, once https://github.com/launchbadge/sqlx/issues/3071 is taken care of,
 *  these models can be re-leveraged for doing inserts as well (well, more than
 *  likely there will be insert-specific models, but they'd go in here).
 *
 *  There are type-specific primary/foreign key IDs to make it more explicit
 *  what type of key is being passed around. A bunch of uuid::Uuid is meh.
 */

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use carbide_uuid::DbTable;
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::{
    MeasurementApprovedMachineId, MeasurementApprovedProfileId, MeasurementBundleId,
    MeasurementBundleValueId, MeasurementJournalId, MeasurementReportId, MeasurementReportValueId,
    MeasurementSystemProfileAttrId, MeasurementSystemProfileId, TrustedMachineId,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[cfg(feature = "sqlx")]
use sqlx::{
    postgres::PgRow,
    {FromRow, Row},
};

use crate::DisplayName;
#[cfg(feature = "cli")]
use crate::{ToTable, serde_just_print_summary};

/// StringToEnumError is used for taking an input string and converting
/// it to an enum of a given type. It is leveraged by MeasurementBundleState,
/// MeasurementApprovedType, and anything else that might need
/// to leverage it further.
#[derive(Debug)]
pub struct StringToEnumError;

impl fmt::Display for StringToEnumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to convert measurement bundle state to enum")
    }
}

impl Error for StringToEnumError {}

/// MeasurementSystemProfileRecord defines a single row from the
/// measurement_system_profiles table in the database.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as ToTable for printing out details via prettytable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementSystemProfileRecord {
    // profile_id is the auto-generated UUID assigned to the profile,
    // and internally typed as a MeasurementSystemProfileId.
    pub profile_id: MeasurementSystemProfileId,

    // name is the [db-enforced] unique, human-friendly name for the
    // profile. for manually-created profiles, it is expected that
    // a name is provided. for auto-created profiles, some sort of
    // derived name is generated.
    pub name: String,

    // ts is the timestamp the profile was created.
    pub ts: DateTime<Utc>,
}

impl DbTable for MeasurementSystemProfileRecord {
    fn db_table_name() -> &'static str {
        "measurement_system_profiles"
    }
}

impl DisplayName for MeasurementSystemProfileRecord {
    fn display_name() -> &'static str {
        "system profile record"
    }
}

/// MeasurementSystemProfileAttrRecord defines a single row from
/// the measurement_system_profiles_attrs table in the database.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementSystemProfileAttrRecord {
    // attribute_id is the auto-generated UUID assigned to this
    // specific attribute record for its profile attributes.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub attribute_id: MeasurementSystemProfileAttrId,

    // profile_id is the system profile ID that this specific
    // attribute is a part of.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub profile_id: MeasurementSystemProfileId,

    // key is the attribute key (e.g. vendor, product, etc), and
    // is generally derived from some sort of value that comes
    // from DiscoveryInfo.
    pub key: String,

    // value is the value for the attribute, again being generally
    // derived from sort of value coming from DiscoveryInfo.
    pub value: String,

    // ts is the timestamp this record was created.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub ts: chrono::DateTime<Utc>,
}

impl DbTable for MeasurementSystemProfileAttrRecord {
    fn db_table_name() -> &'static str {
        "measurement_system_profiles_attrs"
    }
}

impl DisplayName for MeasurementSystemProfileAttrRecord {
    fn display_name() -> &'static str {
        "system profile attr record"
    }
}

/// MeasurementBundleState is an enum in the database, and
/// is used for tracking the state of a measurement bundle.
///
/// Impls FromStr trait.
#[derive(Copy, Debug, Eq, Hash, PartialEq, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "measurement_bundle_state", rename_all = "lowercase")
)]
pub enum MeasurementBundleState {
    // Pending exists such that, when a bundle is created, it has the
    // option of needing approval to become Active before machines can
    // start passing measurements against it. In that case, the bundle
    // is marked as Pending.
    Pending,

    // Active is used to notate a an active bundle to which machines
    // will be considered Measured when matching against this bundle.
    Active,

    // Obsolete is used to note a deprecated bundle. It's still allowed
    // to match for attestation, but those machines will be reported
    // as being on obsolete measurements (and need to upgrade, for example).
    Obsolete,

    // Retired is used to notate, well, a retired bundle. Machines matching
    // a Retied bundle will be considered MeasuringFailed, and not pass
    // measurements.
    //
    // Retired bundles CAN have their state changed (i.e. back to Pending,
    // Active, Obsolete, etc).
    Retired,

    // Revoked is similar to Retired, in that machines matching a Revoked
    // bundle will be considered MeasuringFailed, and not pass measurements.
    //
    // The purpose of Revoked is generally to mark a very well-known BAD
    // bundle (as in, we discovered an issue with a class of machines with
    // certain measurements), and we NEVER want to pass attestation for
    // those machines.
    //
    // Revoked bundles CANNOT have their state changed.
    Revoked,
}

impl FromStr for MeasurementBundleState {
    type Err = StringToEnumError;

    fn from_str(input: &str) -> Result<MeasurementBundleState, Self::Err> {
        match input {
            "Pending" => Ok(MeasurementBundleState::Pending),
            "Active" => Ok(MeasurementBundleState::Active),
            "Obsolete" => Ok(MeasurementBundleState::Obsolete),
            "Retired" => Ok(MeasurementBundleState::Retired),
            "Revoked" => Ok(MeasurementBundleState::Revoked),
            _ => Err(StringToEnumError),
        }
    }
}

impl fmt::Display for MeasurementBundleState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// MeasurementBundleStateRecord exists so we can do an sqlx::query_as and
/// *just* select the state (and bind it to a struct). It doesn't really need
/// to be much other than this for now.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementBundleStateRecord {
    // Read the comment above, but state is the actual state.
    pub state: MeasurementBundleState,
}

/// MeasurementBundleRecord defines a single row from
/// the measurement_bundles table.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as ToTable for printing out details via prettytable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementBundleRecord {
    // bundle_id is the auto-generated UUID for a measurement bundle,
    // and is used as a reference ID for all measurement_bundle_value
    // records.
    pub bundle_id: MeasurementBundleId,

    // name is the [db-enforced] unique, human-friendly name for the
    // bundle. for manually-created bundles, it is expected that
    // a name is provided. for auto-created bundles, some sort of
    // derived name is generated.
    pub name: String,

    // profile_id is the system profile this bundle is associated
    // with, allowing us to track bundles per profile.
    pub profile_id: MeasurementSystemProfileId,

    // state is the state of this bundle.
    // See the MeasurementBundleState enum for more info,
    // including all states, and what they mean.
    pub state: MeasurementBundleState,

    // ts is the timestamp this record was created.
    pub ts: chrono::DateTime<Utc>,
}

impl DbTable for MeasurementBundleRecord {
    fn db_table_name() -> &'static str {
        "measurement_bundles"
    }
}

impl DisplayName for MeasurementBundleRecord {
    fn display_name() -> &'static str {
        "bundle record"
    }
}

/// MeasurementBundleValueRecord defines a single row
/// from the measurement_bundles_values table.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementBundleValueRecord {
    // value_id is the auto-generated UUID for this record.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub value_id: MeasurementBundleValueId,

    // bundle_id is the ID of the measurement bundle this
    // value is associated with.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub bundle_id: MeasurementBundleId,

    // pcr_register is the specific PCR register index (starting
    // at 0) that the corresponding sha256 is from.
    pub pcr_register: i16,

    // sha_any is any shaXXX from the PCR register.
    pub sha_any: String,

    // ts is the timestamp the record was created.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub ts: chrono::DateTime<Utc>,
}

impl DbTable for MeasurementBundleValueRecord {
    fn db_table_name() -> &'static str {
        "measurement_bundles_values"
    }
}

impl DisplayName for MeasurementBundleValueRecord {
    fn display_name() -> &'static str {
        "bundle value record"
    }
}

/// MeasurementReportRecord defines a single row from
/// the measurement_reports table.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as ToTable for printing out details via prettytable.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementReportRecord {
    // report_id is the auto-generated UUID specific to this report.
    pub report_id: MeasurementReportId,

    // machine_id is the "mock" machine ID that this report is for.
    pub machine_id: MachineId,

    // ts is the timestamp the report record was created.
    pub ts: chrono::DateTime<Utc>,
}

impl DbTable for MeasurementReportRecord {
    fn db_table_name() -> &'static str {
        "measurement_reports"
    }
}

impl DisplayName for MeasurementReportRecord {
    fn display_name() -> &'static str {
        "report record"
    }
}

/// MeasurementReportValueRecord defines a single row from
/// the measurement_reports_values table.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as a self-implementation for converting into a PcrRegisterValue.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementReportValueRecord {
    // value_id is the auto-generated UUID for this value record.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub value_id: MeasurementReportValueId,

    // report_id is the measurement report record this value is
    // associated with.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub report_id: MeasurementReportId,

    // pcr_register is the specific PCR register index (starting
    // at 0) that the corresponding sha_any is from.
    pub pcr_register: i16,

    // sha_any is the sha_any value reported for the given
    // PCR register from the machine.
    pub sha_any: String,

    // ts is the timestamp this record was created.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub ts: chrono::DateTime<Utc>,
}

impl DbTable for MeasurementReportValueRecord {
    fn db_table_name() -> &'static str {
        "measurement_reports_values"
    }
}

impl DisplayName for MeasurementReportValueRecord {
    fn display_name() -> &'static str {
        "report value record"
    }
}

/// MeasurementJournalRecord defines a single row from
/// the measurement_journal table.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as ToTable for printing out details via prettytable.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementJournalRecord {
    // journal is the auto-generated UUID specific to this
    // journal entry.
    pub journal_id: MeasurementJournalId,

    // machine_id is the ID of the machine for this journal
    // entry. Technically this can be derived from report_id,
    // but it makes things easier just having it right here
    // versus needing to join against the reports table.
    pub machine_id: MachineId,

    // report_id is the report record that this journal entry is for.
    pub report_id: MeasurementReportId,

    // profile_id is the matched system profile for the machine
    // that generated the report referenced in this journal.
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub profile_id: Option<MeasurementSystemProfileId>,

    // bundle_id is the matched measurement bundle for this
    // journal entry. If no matching bundle exists, this will
    // be None, and the machine will be "Pending".
    #[cfg_attr(
        feature = "cli",
        serde(skip_serializing_if = "serde_just_print_summary")
    )]
    pub bundle_id: Option<MeasurementBundleId>,

    // state is the resulting state of the machine based on
    // this journal entry. For example, if the machine matches
    // an active bundle, the machine state will be Measured,
    // whereas if a retired (or revoked) bundle is matched,
    // the machine state will be MeasuringFailed. If no bundle
    // is matched, this state will show Pending.
    pub state: MeasurementMachineState,

    // ts is the timestamp the journal record was created.
    pub ts: chrono::DateTime<Utc>,
}

impl DbTable for MeasurementJournalRecord {
    fn db_table_name() -> &'static str {
        "measurement_journal"
    }
}

impl DisplayName for MeasurementJournalRecord {
    fn display_name() -> &'static str {
        "journal record"
    }
}

/// MeasurementMachineState is an enum in the database, and
/// is used for tracking the state of a machine.
///
/// Impls FromStr trait.
#[derive(Copy, Debug, Eq, Hash, PartialEq, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "measurement_machine_state", rename_all = "lowercase")
)]
pub enum MeasurementMachineState {
    Discovered,
    PendingBundle,
    Measured,
    MeasuringFailed,
}

impl FromStr for MeasurementMachineState {
    type Err = StringToEnumError;

    fn from_str(input: &str) -> Result<MeasurementMachineState, Self::Err> {
        match input {
            "Discovered" => Ok(MeasurementMachineState::Discovered),
            "PendingBundle" => Ok(MeasurementMachineState::PendingBundle),
            "Measured" => Ok(MeasurementMachineState::Measured),
            "MeasuringFailed" => Ok(MeasurementMachineState::MeasuringFailed),
            _ => Err(StringToEnumError),
        }
    }
}

impl fmt::Display for MeasurementMachineState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateMachineSummary {
    // machine_id is the ID of the machine, e.g. fm100hxxxxx.
    pub machine_id: MachineId,

    // ts is the timestamp this record was created.
    pub ts: chrono::DateTime<Utc>,
}

impl DisplayName for CandidateMachineSummary {
    fn display_name() -> &'static str {
        "candidate machine record"
    }
}

#[cfg(feature = "cli")]
impl ToTable for CandidateMachineSummary {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["machine_id", self.machine_id]);
        table.add_row(prettytable::row!["created_ts", self.ts]);
        Ok(table.to_string())
    }
}

/// MeasurementApprovedType is an enum in the database, and
/// is used for tracking the state of a site-approved machine that
/// measurements will be auto-approved as a bundle.
///
/// Impls FromStr trait.
#[derive(Copy, Debug, PartialEq, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(
    feature = "sqlx",
    sqlx(type_name = "measurement_approved_type", rename_all = "lowercase")
)]
pub enum MeasurementApprovedType {
    Oneshot,
    Persist,
}

impl FromStr for MeasurementApprovedType {
    type Err = StringToEnumError;

    fn from_str(input: &str) -> Result<MeasurementApprovedType, Self::Err> {
        match input {
            "Oneshot" => Ok(MeasurementApprovedType::Oneshot),
            "Persist" => Ok(MeasurementApprovedType::Persist),
            _ => Err(StringToEnumError),
        }
    }
}

impl fmt::Display for MeasurementApprovedType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// MeasurementApprovedMachineRecord defines a single row from
/// the measurement_approved_machines table.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as ToTable for printing out details via prettytable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementApprovedMachineRecord {
    // approval_id is the auto-generated UUID for this approval record.
    pub approval_id: MeasurementApprovedMachineId,

    // machine_id is the ID of the machine this approval is for.
    pub machine_id: TrustedMachineId,

    // state is the type of approval (oneshot or persist).
    pub approval_type: MeasurementApprovedType,

    // pcr_registers are which PCR registers should be promoted
    // into a bundle from the corresponding report record
    // that we are auto-promoting. This takes the same format
    // as the --pcr-registers CLI flag, which is ultimately
    // parsed by parse_pcr_index_input.
    pub pcr_registers: Option<String>,

    // comments is an optional comment that can be provided with
    // the auto-approval record.
    pub comments: Option<String>,

    // ts is the timestamp the approval record was created.
    pub ts: chrono::DateTime<Utc>,
}

#[cfg(feature = "sqlx")]
impl<'r> FromRow<'r, PgRow> for MeasurementApprovedMachineRecord {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let id_str: &str = row.try_get("machine_id")?;
        let machine_id =
            TrustedMachineId::from_str(id_str).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        Ok(Self {
            approval_id: row.try_get("approval_id")?,
            machine_id,
            approval_type: row.try_get("approval_type")?,
            pcr_registers: row.try_get("pcr_registers")?,
            comments: row.try_get("comments")?,
            ts: row.try_get("ts")?,
        })
    }
}

impl DisplayName for MeasurementApprovedMachineRecord {
    fn display_name() -> &'static str {
        "record"
    }
}

impl DbTable for MeasurementApprovedMachineRecord {
    fn db_table_name() -> &'static str {
        "measurement_approved_machines"
    }
}

#[cfg(feature = "cli")]
impl ToTable for MeasurementApprovedMachineRecord {
    fn into_table(self) -> eyre::Result<String> {
        let pcr_registers: String = match self.pcr_registers.clone() {
            Some(pcr_registers) => pcr_registers,
            None => "".to_string(),
        };
        let comments: String = match self.comments.clone() {
            Some(comments) => comments,
            None => "".to_string(),
        };
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["approval_id", self.approval_id]);
        table.add_row(prettytable::row!["machine_id", self.machine_id]);
        table.add_row(prettytable::row!["approval_type", self.approval_type]);
        table.add_row(prettytable::row!["created_ts", self.ts]);
        table.add_row(prettytable::row!["pcr_registers", pcr_registers]);
        table.add_row(prettytable::row!["comments", comments]);
        Ok(table.to_string())
    }
}
/// MeasurementApprovedProfileRecord defines a single row from
/// the measurement_approved_profiles table.
///
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as ToTable for printing out details via prettytable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(FromRow))]
pub struct MeasurementApprovedProfileRecord {
    // approval_id is the auto-generated UUID for this approval record.
    pub approval_id: MeasurementApprovedProfileId,

    // profile_id is the profile this approval is for.
    pub profile_id: MeasurementSystemProfileId,

    // state is the type of approval (oneshot or persist).
    pub approval_type: MeasurementApprovedType,

    // pcr_registers are which PCR registers should be promoted
    // into a bundle from the corresponding report record
    // that we are auto-promoting. This takes the same format
    // as the --pcr-registers CLI flag, which is ultimately
    // parsed by parse_pcr_index_input.
    pub pcr_registers: Option<String>,

    // comments is an optional comment that can be provided with
    // the auto-approval record.
    pub comments: Option<String>,

    // ts is the timestamp the approval record was created.
    pub ts: chrono::DateTime<Utc>,
}

impl DbTable for MeasurementApprovedProfileRecord {
    fn db_table_name() -> &'static str {
        "measurement_approved_profiles"
    }
}

impl DisplayName for MeasurementApprovedProfileRecord {
    fn display_name() -> &'static str {
        "record"
    }
}

#[cfg(feature = "cli")]
impl ToTable for MeasurementApprovedProfileRecord {
    fn into_table(self) -> eyre::Result<String> {
        let pcr_registers: String = match self.pcr_registers.clone() {
            Some(pcr_registers) => pcr_registers,
            None => "".to_string(),
        };
        let comments: String = match self.comments.clone() {
            Some(comments) => comments,
            None => "".to_string(),
        };
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["approval_id", self.approval_id]);
        table.add_row(prettytable::row!["profile_id", self.profile_id]);
        table.add_row(prettytable::row!["approval_type", self.approval_type]);
        table.add_row(prettytable::row!["created_ts", self.ts]);
        table.add_row(prettytable::row!["pcr_registers", pcr_registers]);
        table.add_row(prettytable::row!["comments", comments]);
        Ok(table.to_string())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, check_cases};

    use super::*;

    #[derive(Clone, Copy)]
    enum RecordEnum {
        BundleState,
        MachineState,
        ApprovalType,
    }

    struct EnumInput {
        enum_type: RecordEnum,
        value: &'static str,
    }

    #[derive(Debug, PartialEq)]
    enum ParsedRecordEnum {
        BundleState(MeasurementBundleState),
        MachineState(MeasurementMachineState),
        ApprovalType(MeasurementApprovedType),
    }

    #[derive(Debug, PartialEq)]
    struct EnumSummary {
        parsed: ParsedRecordEnum,
        display: String,
    }

    #[test]
    fn enum_parse_and_display_cases() {
        check_cases(
            [
                Case {
                    scenario: "pending bundle",
                    input: EnumInput {
                        enum_type: RecordEnum::BundleState,
                        value: "Pending",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::BundleState(MeasurementBundleState::Pending),
                        display: "Pending".to_string(),
                    }),
                },
                Case {
                    scenario: "active bundle",
                    input: EnumInput {
                        enum_type: RecordEnum::BundleState,
                        value: "Active",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::BundleState(MeasurementBundleState::Active),
                        display: "Active".to_string(),
                    }),
                },
                Case {
                    scenario: "obsolete bundle",
                    input: EnumInput {
                        enum_type: RecordEnum::BundleState,
                        value: "Obsolete",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::BundleState(MeasurementBundleState::Obsolete),
                        display: "Obsolete".to_string(),
                    }),
                },
                Case {
                    scenario: "retired bundle",
                    input: EnumInput {
                        enum_type: RecordEnum::BundleState,
                        value: "Retired",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::BundleState(MeasurementBundleState::Retired),
                        display: "Retired".to_string(),
                    }),
                },
                Case {
                    scenario: "revoked bundle",
                    input: EnumInput {
                        enum_type: RecordEnum::BundleState,
                        value: "Revoked",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::BundleState(MeasurementBundleState::Revoked),
                        display: "Revoked".to_string(),
                    }),
                },
                Case {
                    scenario: "unknown bundle state",
                    input: EnumInput {
                        enum_type: RecordEnum::BundleState,
                        value: "Unknown",
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "discovered machine",
                    input: EnumInput {
                        enum_type: RecordEnum::MachineState,
                        value: "Discovered",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::MachineState(MeasurementMachineState::Discovered),
                        display: "Discovered".to_string(),
                    }),
                },
                Case {
                    scenario: "machine pending bundle",
                    input: EnumInput {
                        enum_type: RecordEnum::MachineState,
                        value: "PendingBundle",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::MachineState(
                            MeasurementMachineState::PendingBundle,
                        ),
                        display: "PendingBundle".to_string(),
                    }),
                },
                Case {
                    scenario: "measured machine",
                    input: EnumInput {
                        enum_type: RecordEnum::MachineState,
                        value: "Measured",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::MachineState(MeasurementMachineState::Measured),
                        display: "Measured".to_string(),
                    }),
                },
                Case {
                    scenario: "failed measurement",
                    input: EnumInput {
                        enum_type: RecordEnum::MachineState,
                        value: "MeasuringFailed",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::MachineState(
                            MeasurementMachineState::MeasuringFailed,
                        ),
                        display: "MeasuringFailed".to_string(),
                    }),
                },
                Case {
                    scenario: "unknown machine state",
                    input: EnumInput {
                        enum_type: RecordEnum::MachineState,
                        value: "Unknown",
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "one-shot approval",
                    input: EnumInput {
                        enum_type: RecordEnum::ApprovalType,
                        value: "Oneshot",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::ApprovalType(MeasurementApprovedType::Oneshot),
                        display: "Oneshot".to_string(),
                    }),
                },
                Case {
                    scenario: "persistent approval",
                    input: EnumInput {
                        enum_type: RecordEnum::ApprovalType,
                        value: "Persist",
                    },
                    expect: Yields(EnumSummary {
                        parsed: ParsedRecordEnum::ApprovalType(MeasurementApprovedType::Persist),
                        display: "Persist".to_string(),
                    }),
                },
                Case {
                    scenario: "unknown approval type",
                    input: EnumInput {
                        enum_type: RecordEnum::ApprovalType,
                        value: "Unknown",
                    },
                    expect: Fails,
                },
            ],
            |input| {
                match input.enum_type {
                    RecordEnum::BundleState => {
                        input
                            .value
                            .parse::<MeasurementBundleState>()
                            .map(|value| EnumSummary {
                                display: value.to_string(),
                                parsed: ParsedRecordEnum::BundleState(value),
                            })
                    }
                    RecordEnum::MachineState => {
                        input
                            .value
                            .parse::<MeasurementMachineState>()
                            .map(|value| EnumSummary {
                                display: value.to_string(),
                                parsed: ParsedRecordEnum::MachineState(value),
                            })
                    }
                    RecordEnum::ApprovalType => {
                        input
                            .value
                            .parse::<MeasurementApprovedType>()
                            .map(|value| EnumSummary {
                                display: value.to_string(),
                                parsed: ParsedRecordEnum::ApprovalType(value),
                            })
                    }
                }
                .map_err(drop)
            },
        );
    }
}
