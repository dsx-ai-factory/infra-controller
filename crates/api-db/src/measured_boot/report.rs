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

use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::{
    MeasurementBundleId, MeasurementReportId, MeasurementSystemProfileId, TrustedMachineId,
};
use measured_boot::bundle::MeasurementBundle;
use measured_boot::journal::MeasurementJournal;
use measured_boot::pcr::{PcrRegisterValue, PcrSet, parse_pcr_index_input};
use measured_boot::records::{
    MeasurementApprovedType, MeasurementBundleState, MeasurementMachineState,
    MeasurementReportRecord, MeasurementReportValueRecord,
};
use measured_boot::report::MeasurementReport;
use sqlx::{PgConnection, PgTransaction};

use crate::db_read::DbReader;
use crate::measured_boot::interface::common;
use crate::measured_boot::interface::common::pcr_register_values_to_map;
use crate::measured_boot::interface::report::{
    delete_report_for_id, delete_report_values_for_id, get_measurement_report_record_by_id,
    get_measurement_report_values_for_report_id, insert_measurement_report_record,
    insert_measurement_report_value_records, update_report_tstamp, update_report_values_tstamp,
};
use crate::measured_boot::interface::site::{
    get_approval_for_machine_id, get_approval_for_profile_id,
    remove_from_approved_machines_by_approval_id, remove_from_approved_profiles_by_approval_id,
};
use crate::measured_boot::machine::bundle_state_to_machine_state;
use crate::{DatabaseError, DatabaseResult};

pub async fn new(
    txn: &mut PgTransaction<'_>,
    machine_id: MachineId,
    values: &[PcrRegisterValue],
) -> DatabaseResult<MeasurementReport> {
    create_measurement_report(txn, machine_id, values).await
}

pub async fn from_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> DatabaseResult<MeasurementReport> {
    get_measurement_report_by_id(txn, report_id).await
}

/// delete_for_id deletes a MeasurementReport and associated
/// MeasurementReportValues, returning a fully populated instance of
/// MeasurementReport of the data that was deleted for `report_id`.
pub async fn delete_for_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> DatabaseResult<MeasurementReport> {
    let values = delete_report_values_for_id(txn, report_id).await?;
    match delete_report_for_id(txn, report_id).await? {
        Some(info) => Ok(MeasurementReport {
            report_id: info.report_id,
            machine_id: info.machine_id,
            ts: info.ts,
            values,
        }),
        None => Err(DatabaseError::NotFoundError {
            kind: "MeasurementReport",
            id: report_id.to_string(),
        }),
    }
}

pub async fn get_all_for_machine_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<Vec<MeasurementReport>> {
    get_measurement_reports_for_machine_id(txn, machine_id).await
}

pub async fn get_all<DB>(txn: &mut DB) -> DatabaseResult<Vec<MeasurementReport>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    get_all_measurement_reports(txn).await
}

pub async fn create_active_bundle(
    txn: &mut PgTransaction<'_>,
    report: &MeasurementReport,
    pcr_set: &Option<PcrSet>,
) -> DatabaseResult<MeasurementBundle> {
    create_bundle_with_state(txn, report, MeasurementBundleState::Active, pcr_set).await
}

pub async fn create_revoked_bundle(
    txn: &mut PgTransaction<'_>,
    report: &MeasurementReport,
    pcr_set: &Option<PcrSet>,
) -> DatabaseResult<MeasurementBundle> {
    create_bundle_with_state(txn, report, MeasurementBundleState::Revoked, pcr_set).await
}

/// create_measurement_report handles the work of creating a new
/// measurement report as well as all associated value records.
pub async fn create_measurement_report(
    txn: &mut PgTransaction<'_>,
    machine_id: MachineId,
    values: &[PcrRegisterValue],
) -> DatabaseResult<MeasurementReport> {
    // we check if a previous measurement report has the same values,
    // if true, we'll just update the timestamp on the previous report
    // otherwise, we'll create a new one
    let (info, values, new_report_created) =
        match same_as_previous_one(txn, machine_id, values).await? {
            SameOrNot::Same(report_id) => {
                // update the timestamps
                let now = chrono::Utc::now();

                let info = update_report_tstamp(txn, report_id, now).await?;
                let values = update_report_values_tstamp(txn, report_id, now).await?;
                (info, values, false)
            }
            SameOrNot::Different => {
                let info = insert_measurement_report_record(txn, machine_id).await?;
                let values =
                    insert_measurement_report_value_records(txn, info.report_id, values).await?;
                (info, values, true)
            }
        };

    let report = MeasurementReport {
        report_id: info.report_id,
        machine_id: info.machine_id,
        ts: info.ts,
        values,
    };

    let journal_data =
        JournalData::new_from_values(txn, report.machine_id, &report.pcr_values()).await?;

    // Now that the bundle_id and profile_id bits have been sorted, its
    // time to make a new journal entry that captures the [possible]
    // bundle_id, the profile_id, and the values to log to the journal.
    let journal = if new_report_created {
        crate::measured_boot::journal::new(
            txn,
            report.machine_id,
            report.report_id,
            journal_data.profile_id,
            journal_data.bundle_id,
            journal_data.state,
        )
        .await?
    } else {
        crate::measured_boot::journal::update_measurement_journal(
            txn,
            report.report_id,
            journal_data.profile_id,
            journal_data.bundle_id,
            journal_data.state,
        )
        .await?
    };

    // TODO(chet): Now that profiles are auto-created if a matching one
    // doesn't exist, maybe this can go, but i'm keeping it here as a
    // placeholder, just incase we want to turn off auto-creation of
    // profiles (or make it configurable).
    if journal.profile_id.is_none() {
        return Err(DatabaseError::MissingArgument("journal.profile_id"));
    }

    // And, finally, if there's no bundle_id associated with the journal entry,
    // see if any sort of auto-approve is configured for the current machine ID.
    // If it is, then convert the journal into an active bundle, and then create
    // a second journal entry backed by the new active bundle.
    //
    // Machine auto-approvals take priority over profile auto-approvals.
    if journal.bundle_id.is_none() && !maybe_auto_approve_machine(txn, &report).await? {
        maybe_auto_approve_profile(txn, &journal, &report).await?;
    }

    Ok(report)
}

/// get_measurement_reports returns all MeasurementReport
/// instances in the database. This leverages the generic get_all_objects
/// function since its a simple/common pattern.
pub async fn get_all_measurement_reports<DB>(txn: &mut DB) -> DatabaseResult<Vec<MeasurementReport>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let report_records: Vec<MeasurementReportRecord> = common::get_all_objects(&mut *txn).await?;
    let mut report_values: Vec<MeasurementReportValueRecord> = common::get_all_objects(txn).await?;

    let mut values_by_report_id: HashMap<MeasurementReportId, Vec<MeasurementReportValueRecord>> =
        HashMap::new();

    for report_value in report_values.drain(..) {
        values_by_report_id
            .entry(report_value.report_id)
            .or_default()
            .push(report_value);
    }

    let mut res = Vec::<MeasurementReport>::new();
    for report_record in report_records.iter() {
        let values = values_by_report_id
            .remove(&report_record.report_id)
            .unwrap_or_default();
        res.push(MeasurementReport {
            report_id: report_record.report_id,
            machine_id: report_record.machine_id,
            ts: report_record.ts,
            values: values.to_vec(),
        });
    }
    Ok(res)
}

/// get_measurement_report_by_id does the work of populating a full
/// MeasurementReport instance, with values and all.
pub async fn get_measurement_report_by_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> DatabaseResult<MeasurementReport> {
    match get_measurement_report_record_by_id(txn, report_id).await? {
        Some(info) => {
            let values = get_measurement_report_values_for_report_id(txn, info.report_id).await?;
            Ok(MeasurementReport {
                report_id: info.report_id,
                machine_id: info.machine_id,
                ts: info.ts,
                values,
            })
        }
        None => Err(DatabaseError::NotFoundError {
            kind: "MeasurementReport",
            id: report_id.to_string(),
        }),
    }
}

/// get_measurement_reports_for_machine_id returns all fully populated
/// report instances for a given machine ID, which is used by the
/// `report show` CLI option.
pub async fn get_measurement_reports_for_machine_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<Vec<MeasurementReport>> {
    let report_records: Vec<MeasurementReportRecord> =
        common::get_objects_where_id(&mut *txn, machine_id).await?;
    let mut res = Vec::<MeasurementReport>::new();
    for report_record in report_records.iter() {
        let values =
            get_measurement_report_values_for_report_id(txn, report_record.report_id).await?;
        res.push(MeasurementReport {
            report_id: report_record.report_id,
            machine_id: report_record.machine_id,
            ts: report_record.ts,
            values,
        });
    }
    Ok(res)
}

/// JournalData is just a small struct used to store data collected as
/// part of attestation work when forming a new journal entry.
struct JournalData {
    state: MeasurementMachineState,
    bundle_id: Option<MeasurementBundleId>,
    profile_id: Option<MeasurementSystemProfileId>,
}

impl JournalData {
    async fn new_from_values(
        txn: &mut PgTransaction<'_>,
        machine_id: MachineId,
        values: &[PcrRegisterValue],
    ) -> DatabaseResult<Self> {
        let state: MeasurementMachineState;
        let bundle_id: Option<MeasurementBundleId>;

        let machine = crate::measured_boot::machine::from_id(txn, machine_id).await?;
        let discovery_attributes = crate::measured_boot::machine::discovery_attributes(&machine)?;
        let profile =
            crate::measured_boot::profile::match_from_attrs_or_new(txn, &discovery_attributes)
                .await?;

        match crate::measured_boot::bundle::match_from_values(txn, profile.profile_id, values)
            .await?
        {
            Some(bundle) => {
                state = bundle_state_to_machine_state(&bundle.state);
                bundle_id = Some(bundle.bundle_id);
            }
            None => {
                state = MeasurementMachineState::PendingBundle;
                bundle_id = None;
            }
        }

        Ok(Self {
            state,
            bundle_id,
            profile_id: Some(profile.profile_id),
        })
    }
}

/// maybe_auto_approve_machine will check to see if an auto-approve config
/// exists for the current machine ID (or ANY machine ID, via "*"). If it
/// does, it will make a new measurement bundle using the selected report
/// registers per the auto approve config.
///
/// It's worth mentioning that this in and of itself will create an additional
/// journal entry, should a new bundle be created.
async fn maybe_auto_approve_machine(
    txn: &mut PgTransaction<'_>,
    report: &MeasurementReport,
) -> DatabaseResult<bool> {
    match get_approval_for_machine_id(txn, TrustedMachineId::MachineId(report.machine_id)).await? {
        Some(approval) => {
            let pcr_set = match approval.pcr_registers {
                Some(pcr_registers) => Some(parse_pcr_index_input(pcr_registers.as_str())?),
                None => None,
            };
            let _ = create_active_bundle(txn, report, &pcr_set).await?;

            // If this is a oneshot approval, then remove the approval
            // entry after this automatic journal promotion.
            if approval.approval_type == MeasurementApprovedType::Oneshot {
                remove_from_approved_machines_by_approval_id(txn, approval.approval_id).await?;
            }
            Ok(true)
        }
        // If there's no matching approval for the specific machine ID,
        // then check to see if there's a "permissive" approval for all
        // machines (which is just a "*"). The permissive approval still
        // has the same rules as a machine-specific approval (oneshot vs.
        // persist, PCR subset limits, etc).
        None => match get_approval_for_machine_id(txn, TrustedMachineId::Any).await? {
            Some(approval) => {
                let pcr_set = match approval.pcr_registers {
                    Some(pcr_registers) => Some(parse_pcr_index_input(pcr_registers.as_str())?),
                    None => None,
                };
                let _ = create_active_bundle(txn, report, &pcr_set).await?;

                // If this is a oneshot approval, then remove the approval
                // entry after this automatic journal promotion.
                if approval.approval_type == MeasurementApprovedType::Oneshot {
                    remove_from_approved_machines_by_approval_id(txn, approval.approval_id).await?;
                }
                Ok(true)
            }
            None => Ok(false),
        },
    }
}

/// maybe_auto_approve_profile will check to see if an auto-approve config
/// exists for the current machine's system profile. If it does, it will make
/// a new measurement bundle using the selected report registers per the auto
/// approve config.
///
/// It's worth mentioning that this in and of itself will create an additional
/// journal entry, should a new bundle be created.
async fn maybe_auto_approve_profile(
    txn: &mut PgTransaction<'_>,
    journal: &MeasurementJournal,
    report: &MeasurementReport,
) -> DatabaseResult<bool> {
    match get_approval_for_profile_id(txn, journal.profile_id.unwrap()).await? {
        Some(approval) => {
            let pcr_set = match approval.pcr_registers {
                Some(pcr_registers) => Some(parse_pcr_index_input(pcr_registers.as_str())?),
                None => None,
            };
            let _ = create_active_bundle(txn, report, &pcr_set).await?;

            // If this is a oneshot approval, then remove the approval
            // entry after this automatic journal promotion.
            if approval.approval_type == MeasurementApprovedType::Oneshot {
                remove_from_approved_profiles_by_approval_id(txn, approval.approval_id).await?;
            }
            Ok(true)
        }
        None => Ok(false),
    }
}

////////////////////////////////////////////////////////////
/// create_bundle_with_state creates a new measurement bundle
/// out of a measurement report with the given state, with
/// the option to provide a range of PCR register values to
/// specifically select from the report's bundle (e.g. maybe
/// the report has registers 0-12, but we only want to
/// create a new bundle with registers 0-6).
////////////////////////////////////////////////////////////
pub async fn create_bundle_with_state(
    txn: &mut PgTransaction<'_>,
    report: &MeasurementReport,
    state: MeasurementBundleState,
    pcr_set: &Option<PcrSet>,
) -> DatabaseResult<MeasurementBundle> {
    // Get machine + profile information for the journal entry
    // that needs to be associated with the bundle change.
    let machine = crate::measured_boot::machine::from_id(txn, report.machine_id).await?;
    let discovery_attributes = crate::measured_boot::machine::discovery_attributes(&machine)?;
    let profile =
        crate::measured_boot::profile::match_from_attrs_or_new(txn, &discovery_attributes).await?;

    // Convert the input MeasurementReportValueRecord entries
    // into a list of PcrRegisterValue entries for the purpose
    // of creating a new bundle.
    let register_map = pcr_register_values_to_map(&report.pcr_values())?;

    let values: Vec<PcrRegisterValue> = match pcr_set {
        // If a pcr_range is provided, make sure its a valid range,
        // and then attempt to pluck out a pcr_register value from
        // the register_map for each index in the range.
        Some(pcr_set) => {
            let filtered: DatabaseResult<Vec<PcrRegisterValue>> = pcr_set
                .iter()
                .map(|pcr_register| match register_map.get(pcr_register) {
                    Some(register_val) => Ok(register_val.clone()),
                    None => Err(DatabaseError::NotFoundError {
                        kind: "PcrRegisterValue",
                        id: pcr_register.to_string(),
                    }),
                })
                .collect();
            filtered?
        }
        // If no pcr_range is provided, then just take all measurement
        // journal values from here and turn them into a new bundle.
        None => report.pcr_values(),
    };

    crate::measured_boot::bundle::new(txn, profile.profile_id, None, &values, Some(state)).await
}

enum SameOrNot {
    Same(MeasurementReportId),
    Different,
}

async fn same_as_previous_one(
    txn: &mut PgConnection,
    machine_id: MachineId,
    values: &[PcrRegisterValue],
) -> DatabaseResult<SameOrNot> {
    let latest_journal =
        match crate::measured_boot::journal::get_latest_journal_for_id(&mut *txn, machine_id)
            .await?
        {
            Some(journal) => journal,
            None => return Ok(SameOrNot::Different),
        };

    let latest_report = get_measurement_report_by_id(txn, latest_journal.report_id).await?;

    let mut incoming_values = values.to_vec();
    incoming_values.sort_by_key(|e| e.pcr_register);

    let mut previous_values = latest_report.pcr_values();
    previous_values.sort_by_key(|e| e.pcr_register);

    if incoming_values == previous_values {
        Ok(SameOrNot::Same(latest_journal.report_id))
    } else {
        Ok(SameOrNot::Different)
    }
}
