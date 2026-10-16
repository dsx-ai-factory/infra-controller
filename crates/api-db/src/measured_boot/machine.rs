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

use carbide_uuid::machine::{MachineId, MachineType};
use chrono::Utc;
use measured_boot::journal::MeasurementJournal;
use measured_boot::machine::CandidateMachine;
use measured_boot::records::{MeasurementBundleState, MeasurementMachineState};
use model::machine::topology::TopologyData;
use serde::Serialize;
use sqlx::{FromRow, PgConnection};

use crate::db_read::DbReader;
use crate::measured_boot::interface::bundle::get_measurement_bundle_by_id;
use crate::measured_boot::interface::common;
use crate::measured_boot::interface::machine::{
    get_candidate_machine_record_by_id, get_candidate_machine_records, get_candidate_machine_state,
};
use crate::measured_boot::journal::get_latest_journal_for_id;
use crate::{DatabaseError, DatabaseResult};

/// CandidateMachineRecord defines a single row from
/// the machine_topologies table. Sort of. Where other records
/// implement the whole record, this is just a partial match
/// for the purpose of more easily migrating from the PoC MockMachines
/// code to the production CandidateMachines code. This *could* pull
/// the whole row, but we don't really
/// Impls DbTable trait for generic selects defined in db/interface/common.rs,
/// as well as ToTable for printing out details via prettytable.
#[derive(Debug, Clone, Serialize, FromRow, carbide_macros::DbTable)]
#[db_table(name = "machine_topologies")]
pub struct CandidateMachineRecord {
    // machine_id is the ID of the machine, e.g. fm100hxxxxx.
    pub machine_id: MachineId,

    // topology is the topology JSON blob.
    pub topology: sqlx::types::Json<TopologyData>,

    // created is the timestamp this record was created.
    pub created: chrono::DateTime<Utc>,

    // updated is the timestamp this record was updated.
    pub updated: chrono::DateTime<Utc>,
}

pub async fn from_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<CandidateMachine> {
    match get_candidate_machine_record_by_id(txn, machine_id).await? {
        Some(record) => {
            let attrs = match &record.topology.discovery_data.info.dmi_data {
                Some(dmi_data) => Ok(HashMap::from([
                    (String::from("sys_vendor"), dmi_data.sys_vendor.clone()),
                    (String::from("product_name"), dmi_data.product_name.clone()),
                    (String::from("bios_version"), dmi_data.bios_version.clone()),
                ])),
                None => Err(DatabaseError::internal(String::from(
                    "machine missing dmi data",
                ))),
            }?;

            let journal = get_latest_journal_for_id(txn, machine_id).await?;
            let state = machine_state_from_journal(&journal);

            Ok(CandidateMachine {
                machine_id: record.machine_id,
                created_ts: record.created,
                updated_ts: record.updated,
                attrs,
                state,
                journal,
            })
        }
        None => Err(DatabaseError::NotFoundError {
            kind: "CandidateMachine",
            id: machine_id.to_string(),
        }),
    }
}

pub async fn get_all<DB>(txn: &mut DB) -> DatabaseResult<Vec<CandidateMachine>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    get_candidate_machines(txn).await
}

/// discovery_attributes returns the mock machine attribute
/// records into a generic "discovery attributes" hashmap,
/// which is intended for making the transition from this PoC
/// to actual discovery data easier.
pub fn discovery_attributes(machine: &CandidateMachine) -> DatabaseResult<HashMap<String, String>> {
    common::filter_machine_discovery_attrs(&machine.attrs)
}

pub fn bundle_state_to_machine_state(
    bundle_state: &MeasurementBundleState,
) -> MeasurementMachineState {
    match bundle_state {
        MeasurementBundleState::Active => MeasurementMachineState::Measured,
        MeasurementBundleState::Obsolete => MeasurementMachineState::Measured,
        MeasurementBundleState::Retired => MeasurementMachineState::MeasuringFailed,
        MeasurementBundleState::Revoked => MeasurementMachineState::MeasuringFailed,
        MeasurementBundleState::Pending => MeasurementMachineState::PendingBundle,
    }
}

/// get_measurement_machine_state figures out the current state of the given
/// machine ID by checking its most recent bundle (or lack thereof), and
/// using that result to give it a corresponding MeasurementMachineState.
pub async fn get_measurement_machine_state(
    db_reader: impl DbReader<'_>,
    machine_id: MachineId,
) -> Result<MeasurementMachineState, DatabaseError> {
    get_candidate_machine_state(db_reader, machine_id).await
}

/// get_measurement_bundle_state returns the state of the current bundle
/// associated with the machine, if one exists.
pub async fn get_measurement_bundle_state<DB>(
    db_reader: &mut DB,
    machine_id: &MachineId,
) -> eyre::Result<Option<MeasurementBundleState>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let result = get_latest_journal_for_id(&mut *db_reader, *machine_id).await?;
    if let Some(journal_record) = result
        && let Some(bundle_id) = journal_record.bundle_id
        && let Some(bundle) = get_measurement_bundle_by_id(db_reader, bundle_id).await?
    {
        return Ok(Some(bundle.state));
    }
    Ok(None)
}

/// get_candidate_machines returns all populated CandidateMachine instances.
async fn get_candidate_machines<DB>(txn: &mut DB) -> DatabaseResult<Vec<CandidateMachine>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let mut res: Vec<CandidateMachine> = Vec::new();
    let mut records = get_candidate_machine_records(&mut *txn).await?;

    for record in records.drain(..) {
        // there is no dmi_data for predicted hosts, so skip them
        if record.machine_id.machine_type() == MachineType::PredictedHost {
            continue;
        }
        let attrs = match &record.topology.discovery_data.info.dmi_data {
            Some(dmi_data) => Ok(HashMap::from([
                (String::from("sys_vendor"), dmi_data.sys_vendor.clone()),
                (String::from("product_name"), dmi_data.product_name.clone()),
                (String::from("bios_version"), dmi_data.bios_version.clone()),
            ])),
            None => Err(DatabaseError::internal(String::from(
                "machine missing dmi data",
            ))),
        }?;

        let journal = get_latest_journal_for_id(&mut *txn, record.machine_id).await?;
        let state = machine_state_from_journal(&journal);

        res.push(CandidateMachine {
            machine_id: record.machine_id,
            created_ts: record.created,
            updated_ts: record.updated,
            attrs,
            state,
            journal,
        });
    }
    Ok(res)
}

/// machine_state_from_journal returns the computed machine
/// state for a given journal record.
pub fn machine_state_from_journal(journal: &Option<MeasurementJournal>) -> MeasurementMachineState {
    match journal {
        Some(record) => record.state,
        None => MeasurementMachineState::Discovered,
    }
}
