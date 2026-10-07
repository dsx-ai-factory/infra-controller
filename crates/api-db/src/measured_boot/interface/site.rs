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
 */

use carbide_uuid::measured_boot::{
    MeasurementApprovedMachineId, MeasurementApprovedProfileId, MeasurementSystemProfileId,
    TrustedMachineId,
};
use measured_boot::records::{
    MeasurementApprovedMachineRecord, MeasurementApprovedProfileRecord, MeasurementApprovedType,
};
use measured_boot::site::MachineAttestationSummary;
use sqlx::PgConnection;

use crate::DatabaseError;
use crate::db_read::DbReader;
use crate::measured_boot::interface::common;

pub async fn insert_into_approved_machines(
    txn: &mut PgConnection,
    machine_id: TrustedMachineId,
    approval_type: MeasurementApprovedType,
    pcr_registers: Option<String>,
    comments: Option<String>,
) -> Result<MeasurementApprovedMachineRecord, DatabaseError> {
    let query = "insert into measurement_approved_machines(machine_id, approval_type, pcr_registers, comments) values($1, $2, $3, $4) returning *";
    sqlx::query_as(query)
        .bind(machine_id)
        .bind(approval_type)
        .bind(pcr_registers)
        .bind(comments)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("insert_into_approved_machines", e))
}

pub async fn remove_from_approved_machines_by_approval_id(
    txn: &mut PgConnection,
    approval_id: MeasurementApprovedMachineId,
) -> Result<MeasurementApprovedMachineRecord, DatabaseError> {
    let query = "delete from measurement_approved_machines where approval_id = $1 returning *";
    sqlx::query_as(query)
        .bind(approval_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("remove_from_approved_machines_by_approval_id", e))
}

pub async fn remove_from_approved_machines_by_machine_id(
    txn: &mut PgConnection,
    machine_id: TrustedMachineId,
) -> Result<MeasurementApprovedMachineRecord, DatabaseError> {
    let query = "delete from measurement_approved_machines where machine_id = $1 returning *";
    sqlx::query_as(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("remove_from_approved_machines_by_machine_id", e))
}

pub async fn get_approved_machines(
    txn: impl DbReader<'_>,
) -> Result<Vec<MeasurementApprovedMachineRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_approved_machines"))
}

pub async fn get_approval_for_machine_id(
    txn: &mut PgConnection,
    machine_id: TrustedMachineId,
) -> Result<Option<MeasurementApprovedMachineRecord>, DatabaseError> {
    common::get_object_for_id(txn, machine_id)
        .await
        .map_err(|e| e.with_op_name("get_approval_for_machine_id"))
}

pub async fn insert_into_approved_profiles(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
    approval_type: MeasurementApprovedType,
    pcr_registers: Option<String>,
    comments: Option<String>,
) -> Result<MeasurementApprovedProfileRecord, DatabaseError> {
    let query = "insert into measurement_approved_profiles(profile_id, approval_type, pcr_registers, comments) values($1, $2, $3, $4) returning *";
    sqlx::query_as(query)
        .bind(profile_id)
        .bind(approval_type)
        .bind(pcr_registers)
        .bind(comments)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("insert_into_approved_profiles", e))
}

pub async fn remove_from_approved_profiles_by_approval_id(
    txn: &mut PgConnection,
    approval_id: MeasurementApprovedProfileId,
) -> Result<MeasurementApprovedProfileRecord, DatabaseError> {
    let query = "delete from measurement_approved_profiles where approval_id = $1 returning *";
    sqlx::query_as(query)
        .bind(approval_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("remove_from_approved_profiles_by_approval_id", e))
}

pub async fn remove_from_approved_profiles_by_profile_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<MeasurementApprovedProfileRecord, DatabaseError> {
    let query = "delete from measurement_approved_profiles where profile_id = $1 returning *";
    sqlx::query_as(query)
        .bind(profile_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("remove_from_approved_profiles_by_profile_id", e))
}

pub async fn get_approved_profiles(
    txn: impl DbReader<'_>,
) -> Result<Vec<MeasurementApprovedProfileRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_approved_profiles"))
}

pub async fn get_approval_for_profile_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Option<MeasurementApprovedProfileRecord>, DatabaseError> {
    // TODO(chet): get_object_for_id should become fetch_optional.
    let query = "select * from measurement_approved_profiles where profile_id = $1";
    sqlx::query_as(query)
        .bind(profile_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("get_approval_for_profile_id", e))
}

pub async fn list_attestation_summary(
    txn: impl DbReader<'_>,
) -> Result<Vec<MachineAttestationSummary>, DatabaseError> {
    let query = "select distinct on (mj.machine_id) mj.machine_id, mj.ts, msp.name, mj.bundle_id from measurement_journal mj, measurement_system_profiles msp WHERE mj.profile_id = msp.profile_id order by mj.machine_id, mj.ts desc";

    sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("list_attestation_summary", e))
}
