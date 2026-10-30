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

use carbide_uuid::machine::MachineId;
use config_version::ConfigVersion;
use itertools::Itertools;
use model::attestation::spdm::{
    CaCertificate, Evidence, SpdmAttestationState, SpdmAttestationStatus, SpdmDeviceAttestation,
    SpdmDeviceAttestationDetails, SpdmMachineDeviceMetadata, SpdmObjectId,
};
use model::controller_outcome::PersistentStateHandlerOutcome;
use sqlx::{PgConnection, Row};

use crate::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult};

#[cfg(test)]
mod test_explicit_columns;

/// Inserts a job's device attestations with one shared trigger origin.
/// `triggered_by_mh_controller` is true only for managed-host controller triggers;
/// false also covers attestations whose controller origin is unknown.
pub async fn insert_device_attestations(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    devices: Vec<SpdmDeviceAttestation>,
    job_id: &uuid::Uuid,
    triggered_by_mh_controller: bool,
) -> DatabaseResult<u64> {
    let device_ids = devices.iter().map(|x| &x.device_id).collect_vec();
    let nonces = devices.iter().map(|x| x.nonce).collect_vec();
    let states = devices
        .iter()
        .map(|x| sqlx::types::Json(&x.state))
        .collect_vec();
    let state_versions = devices
        .iter()
        .map(|x| x.state_version.to_string())
        .collect_vec();
    let ca_certificate_links = devices.iter().map(|x| &x.ca_certificate_link).collect_vec();
    let evidence_targets = devices.iter().map(|x| &x.evidence_target).collect_vec();

    let started_at_timestamps = devices.iter().map(|x| &x.started_at).collect_vec();

    // Bind provenance once for the job rather than independently for each device.
    let query = r#"INSERT INTO spdm_machine_devices_attestation (machine_id, device_id, nonce, state, state_version, ca_certificate_link, evidence_target, job_id, started_at, triggered_by_mh_controller)
        SELECT 
            $1 as machine_id, device_id, nonce, state, state_version, ca_certificate_link, evidence_target, $8 as job_id, started_at, $10 as triggered_by_mh_controller
        FROM 
            UNNEST($2::TEXT[], $3::uuid[], $4::JSONB[], $5::TEXT[], $6::TEXT[], $7::TEXT[], $9::timestamptz[])
            AS t(device_id, nonce, state, state_version, ca_certificate_link, evidence_target, started_at)
        "#;
    let res = sqlx::query(query)
        .bind(machine_id)
        .bind(device_ids)
        .bind(nonces)
        .bind(states)
        .bind(state_versions)
        .bind(ca_certificate_links)
        .bind(evidence_targets)
        .bind(job_id)
        .bind(started_at_timestamps)
        .bind(triggered_by_mh_controller)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(res.rows_affected())
}

/// Requests cancellation while preserving the deadline of any earlier request.
pub async fn cancel_attestation_job(
    txn: &mut PgConnection,
    job_id: &uuid::Uuid,
) -> DatabaseResult<()> {
    let current_time = chrono::Utc::now();
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET cancelled_at = COALESCE(cancelled_at, $2)
        WHERE job_id = $1
        "#;
    sqlx::query(query)
        .bind(job_id)
        .bind(current_time)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Claims the shared evidence endpoint for an unclaimed, current attestation.
///
/// Returns `false` if another job holds the claim, this job already holds it, or
/// the attestation is stale, cancelled, completed, or missing. Uses a
/// savepoint so expected contention does not abort the caller's transaction.
/// The caller must commit a successful claim before triggering BMC collection.
pub async fn try_acquire_evidence_collection_lock(
    txn: &mut PgConnection,
    object_id: &SpdmObjectId,
    state_version: ConfigVersion,
) -> DatabaseResult<bool> {
    let query = r#"
        UPDATE spdm_machine_devices_attestation
        SET evidence_collection_locked = TRUE
        WHERE machine_id = $1 AND device_id = $2 AND job_id = $3
            AND state_version = $4 AND NOT evidence_collection_locked
            AND cancelled_at IS NULL AND completed_at IS NULL
    "#;
    let mut savepoint = crate::Transaction::begin_inner(txn).await?;
    let result = sqlx::query(query)
        .bind(object_id.0)
        .bind(&object_id.1)
        .bind(object_id.2)
        .bind(state_version)
        .execute(savepoint.as_pgconn())
        .await;

    match result {
        Ok(result) => {
            savepoint.commit().await?;
            Ok(result.rows_affected() == 1)
        }
        Err(sqlx::Error::Database(error))
            if error.is_unique_violation()
                && error.constraint() == Some("spdm_device_evidence_collection_lock") =>
        {
            savepoint.rollback().await?;
            Ok(false)
        }
        Err(error) => {
            savepoint.rollback().await?;
            Err(DatabaseError::query(query, error))
        }
    }
}

/// Releases only this job's evidence collection claim in the caller's transaction.
///
/// Call after persisting evidence, confirming the BMC task has stopped, or reaching
/// the explicit cancellation timeout. The timeout permits release without confirmation.
/// Releasing an already released or missing claim succeeds without changing other jobs.
pub async fn release_evidence_collection_lock(
    txn: &mut PgConnection,
    object_id: &SpdmObjectId,
) -> DatabaseResult<()> {
    let query = r#"
        UPDATE spdm_machine_devices_attestation
        SET evidence_collection_locked = FALSE
        WHERE machine_id = $1 AND device_id = $2 AND job_id = $3
    "#;
    sqlx::query(query)
        .bind(object_id.0)
        .bind(&object_id.1)
        .bind(object_id.2)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    Ok(())
}

pub async fn set_completed_at(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_id: &str,
    job_id: &uuid::Uuid,
) -> DatabaseResult<()> {
    let current_time = chrono::Utc::now();
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET completed_at = $4
        WHERE machine_id = $1 and device_id = $2 and job_id = $3
        "#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
        .bind(job_id)
        .bind(current_time)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .rows_affected();

    Ok(())
}

pub async fn update_metadata(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_id: &str,
    job_id: &uuid::Uuid,
    metadata: &SpdmMachineDeviceMetadata,
) -> DatabaseResult<()> {
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET metadata = $4
        WHERE machine_id = $1 AND device_id = $2 AND job_id = $3"#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
        .bind(job_id)
        .bind(sqlx::types::Json(metadata))
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .rows_affected();

    Ok(())
}

pub async fn update_certificate(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_id: &str,
    job_id: &uuid::Uuid,
    certificate: &CaCertificate,
) -> DatabaseResult<()> {
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET ca_certificate = $4
        WHERE machine_id = $1 AND device_id = $2 AND job_id = $3"#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
        .bind(job_id)
        .bind(sqlx::types::Json(certificate))
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .rows_affected();

    Ok(())
}

pub async fn update_evidence(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_id: &str,
    job_id: &uuid::Uuid,
    evidence: &Evidence,
) -> DatabaseResult<()> {
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET evidence = $4
        WHERE machine_id = $1 AND device_id = $2 AND job_id = $3"#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
        .bind(job_id)
        .bind(sqlx::types::Json(evidence))
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .rows_affected();

    Ok(())
}

/// returns all device attestations that are not completed
pub async fn find_machine_ids_for_attestation(
    txn: &mut PgConnection,
) -> Result<Vec<SpdmObjectId>, DatabaseError> {
    let query = r#"
        SELECT
            machine_id, device_id, job_id
        FROM spdm_machine_devices_attestation
        WHERE
            completed_at is NULL
    "#;

    let object_ids: Vec<SpdmObjectId> = sqlx::query_as(query)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(object_ids)
}

/// Lists jobs matching the aggregate device status selected by `selector`.
/// Unfinished devices take precedence, followed by failure, cancellation, and all-passed.
/// Each summary includes distinct machine IDs from retained device attestations, sorted by ID.
pub async fn list_all_attestation_statuses(
    txn: &mut PgConnection,
    selector: SpdmAttestationStatus,
) -> Result<Vec<(uuid::Uuid, SpdmAttestationStatus, Vec<MachineId>)>, DatabaseError> {
    // Aggregate membership over the same complete job as its status, before filtering jobs.
    let query = match selector {
        SpdmAttestationStatus::InProgress => {
            r#"
        WITH machine_attestation_statuses AS (
            SELECT
                job_id,
                array_agg(DISTINCT machine_id ORDER BY machine_id) AS machine_ids,
                CASE
                    WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                        THEN 'in_progress'
                    WHEN bool_or(state ? 'Failed')
                        THEN 'failed'
                    WHEN bool_or(state = '"Cancelled"'::jsonb)
                        THEN 'cancelled'
                    WHEN bool_and(state = '"Passed"'::jsonb)
                        THEN 'passed'
                END AS attestation_status
            FROM spdm_machine_devices_attestation
            GROUP BY job_id
        )
        SELECT
            job_id,
            attestation_status,
            machine_ids
        FROM machine_attestation_statuses
        WHERE attestation_status = 'in_progress';
            "#
        }
        SpdmAttestationStatus::Failed => {
            r#"
        WITH machine_attestation_statuses AS (
            SELECT
                job_id,
                array_agg(DISTINCT machine_id ORDER BY machine_id) AS machine_ids,
                CASE
                    WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                        THEN 'in_progress'
                    WHEN bool_or(state ? 'Failed')
                        THEN 'failed'
                    WHEN bool_or(state = '"Cancelled"'::jsonb)
                        THEN 'cancelled'
                    WHEN bool_and(state = '"Passed"'::jsonb)
                        THEN 'passed'
                END AS attestation_status
            FROM spdm_machine_devices_attestation
            GROUP BY job_id
        )
        SELECT
            job_id,
            attestation_status,
            machine_ids
        FROM machine_attestation_statuses
        WHERE attestation_status = 'failed';
        "#
        }
        SpdmAttestationStatus::Cancelled => {
            r#"
        WITH machine_attestation_statuses AS (
            SELECT
                job_id,
                array_agg(DISTINCT machine_id ORDER BY machine_id) AS machine_ids,
                CASE
                    WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                        THEN 'in_progress'
                    WHEN bool_or(state ? 'Failed')
                        THEN 'failed'
                    WHEN bool_or(state = '"Cancelled"'::jsonb)
                        THEN 'cancelled'
                    WHEN bool_and(state = '"Passed"'::jsonb)
                        THEN 'passed'
                END AS attestation_status
            FROM spdm_machine_devices_attestation
            GROUP BY job_id
        )
        SELECT
            job_id,
            attestation_status,
            machine_ids
        FROM machine_attestation_statuses
        WHERE attestation_status = 'cancelled';
            "#
        }
        SpdmAttestationStatus::Passed => {
            r#"
        WITH machine_attestation_statuses AS (
            SELECT
                job_id,
                array_agg(DISTINCT machine_id ORDER BY machine_id) AS machine_ids,
                CASE
                    WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                        THEN 'in_progress'
                    WHEN bool_or(state ? 'Failed')
                        THEN 'failed'
                    WHEN bool_or(state = '"Cancelled"'::jsonb)
                        THEN 'cancelled'
                    WHEN bool_and(state = '"Passed"'::jsonb)
                        THEN 'passed'
                END AS attestation_status
            FROM spdm_machine_devices_attestation
            GROUP BY job_id
        )
        SELECT
            job_id,
            attestation_status,
            machine_ids
        FROM machine_attestation_statuses
        WHERE attestation_status = 'passed';
            "#
        }
    };

    let attestation_status_rows = sqlx::query(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if attestation_status_rows.is_empty() {
        return Ok(std::vec::Vec::new());
    }

    let attestation_statuses = attestation_status_rows
        .iter()
        .map(|elem| {
            let job_id: uuid::Uuid = elem
                .try_get("job_id")
                .map_err(|e| DatabaseError::new(query, e))?;
            let status: String = elem
                .try_get("attestation_status")
                .map_err(|e| DatabaseError::new(query, e))?;

            let machine_ids: Vec<MachineId> = elem
                .try_get("machine_ids")
                .map_err(|e| DatabaseError::new(query, e))?;

            Ok((
                job_id,
                convert_status_str_to_model_status(status.as_str())?,
                machine_ids,
            ))
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;

    Ok(attestation_statuses)
}

/// A machine's predicted status from its latest retained controller-triggered job.
pub struct MachineAttestationSummary {
    /// The machine whose prediction is reported.
    pub machine_id: MachineId,
    /// The job's aggregate for this machine, absent when no controller job is retained.
    pub attestation_status: Option<SpdmAttestationStatus>,
}

/// Reads a page of predictions in ascending machine-ID order without contacting BMCs.
/// Selects one controller job per machine by started_at DESC, then job_id DESC,
/// and aggregates all that machine's device rows in that job. External and older
/// jobs do not contribute. An omitted status filter includes machines without a
/// retained controller job; a supplied status excludes them. Only IDs strictly
/// after `after_machine_id` are considered, or all IDs when it is absent.
/// Returns at most `page_size + 1` matching rows, including a lookahead row so the
/// caller can detect another page. Filtering happens before this limit.
pub async fn find_machine_attestation_statuses(
    txn: &mut PgConnection,
    after_machine_id: Option<MachineId>,
    status: Option<SpdmAttestationStatus>,
    page_size: u32,
) -> DatabaseResult<Vec<MachineAttestationSummary>> {
    let status = status.map(|status| match status {
        SpdmAttestationStatus::InProgress => "in_progress",
        SpdmAttestationStatus::Failed => "failed",
        SpdmAttestationStatus::Cancelled => "cancelled",
        SpdmAttestationStatus::Passed => "passed",
    });
    // Resolve the complete source job before aggregating, so obsolete devices
    // from earlier jobs cannot keep a machine failed. Aggregate within each machine
    // lookup so an ordered scan can stop once the page and lookahead are filled.
    let query = r#"
        SELECT m.id AS machine_id, prediction.attestation_status
        FROM machines m
        LEFT JOIN LATERAL (
            SELECT job_id
            FROM spdm_machine_devices_attestation
            WHERE machine_id = m.id AND triggered_by_mh_controller
            ORDER BY started_at DESC, job_id DESC
            LIMIT 1
        ) latest ON true
        LEFT JOIN LATERAL (
            SELECT CASE
                WHEN bool_or(a.state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (a.state ? 'Failed'))
                    THEN 'in_progress'
                WHEN bool_or(a.state ? 'Failed') THEN 'failed'
                WHEN bool_or(a.state = '"Cancelled"'::jsonb) THEN 'cancelled'
                WHEN bool_and(a.state = '"Passed"'::jsonb) THEN 'passed'
            END AS attestation_status
            FROM spdm_machine_devices_attestation a
            WHERE a.machine_id = m.id AND a.job_id = latest.job_id
        ) prediction ON true
        WHERE ($1::varchar IS NULL OR m.id > $1)
            AND ($2::text IS NULL OR prediction.attestation_status = $2)
        ORDER BY m.id
        LIMIT $3
    "#;
    let rows: Vec<(MachineId, Option<String>)> = sqlx::query_as(query)
        .bind(after_machine_id)
        .bind(status)
        .bind(i64::from(page_size) + 1)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    rows.into_iter()
        .map(|(machine_id, status)| {
            Ok(MachineAttestationSummary {
                machine_id,
                attestation_status: status
                    .as_deref()
                    .map(convert_status_str_to_model_status)
                    .transpose()?,
            })
        })
        .collect()
}

/// Aggregates the latest attestation for each device on a machine.
/// Unfinished devices take precedence, followed by failure, cancellation, and all-passed.
/// `device_ids` limits the aggregate to the selected devices; `None` includes all stored devices.
/// Any selected device without a stored result makes the status `InProgress`.
/// Repeated device IDs are treated as one selection. Returns not found for an empty
/// selection, or when `device_ids` is `None` and the machine has no stored attestations.
pub async fn list_single_machine_attestation_status(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_ids: Option<&[String]>,
) -> Result<SpdmAttestationStatus, DatabaseError> {
    // Check missing requested devices before terminal outcomes, just like unfinished work.
    let query = r#"
        WITH latest_device_attestations AS (
            SELECT DISTINCT ON (machine_id, device_id)
                machine_id,
                device_id,
                state
            FROM spdm_machine_devices_attestation
            WHERE machine_id = $1
                AND ($2::text[] IS NULL OR device_id = ANY($2))
            ORDER BY machine_id, device_id, started_at DESC, job_id DESC
        )
        SELECT
            CASE
                WHEN EXISTS (
                    SELECT 1 FROM unnest($2::text[]) AS selected(device_id)
                    WHERE NOT EXISTS (
                        SELECT 1 FROM latest_device_attestations AS latest
                        WHERE latest.device_id = selected.device_id
                    )
                ) THEN 'in_progress'
                WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                    THEN 'in_progress'
                WHEN bool_or(state ? 'Failed')
                    THEN 'failed'
                WHEN bool_or(state = '"Cancelled"'::jsonb)
                    THEN 'cancelled'
                WHEN bool_and(state = '"Passed"'::jsonb)
                    THEN 'passed'
            END AS attestation_status
        FROM latest_device_attestations
        HAVING count(*) > 0 OR cardinality($2::text[]) > 0
    "#;

    let status = sqlx::query_scalar::<_, String>(query)
        .bind(machine_id)
        .bind(device_ids)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    let Some(status) = status else {
        return Err(DatabaseError::NotFoundError {
            kind: "SPDM Attestation Record",
            id: machine_id.to_string(),
        });
    };

    convert_status_str_to_model_status(status.as_str())
}

/// Aggregates the device states for a job, returning not found if it has no devices.
/// Unfinished devices take precedence, followed by failure, cancellation, and all-passed.
pub async fn get_attestation_job_status(
    txn: &mut PgConnection,
    job_id: &uuid::Uuid,
) -> Result<SpdmAttestationStatus, DatabaseError> {
    let query = r#"
        SELECT
            CASE
                WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                    THEN 'in_progress'
                WHEN bool_or(state ? 'Failed')
                    THEN 'failed'
                WHEN bool_or(state = '"Cancelled"'::jsonb)
                    THEN 'cancelled'
                WHEN bool_and(state = '"Passed"'::jsonb)
                    THEN 'passed'
            END AS attestation_status
        FROM spdm_machine_devices_attestation
        WHERE job_id = $1
        HAVING count(*) > 0
    "#;

    let status = sqlx::query_scalar::<_, String>(query)
        .bind(job_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    let Some(status) = status else {
        return Err(DatabaseError::NotFoundError {
            kind: "SPDM Attestation Job",
            id: job_id.to_string(),
        });
    };

    convert_status_str_to_model_status(status.as_str())
}

fn convert_status_str_to_model_status(
    status: &str,
) -> Result<SpdmAttestationStatus, DatabaseError> {
    match status {
        "in_progress" => Ok(SpdmAttestationStatus::InProgress),
        "failed" => Ok(SpdmAttestationStatus::Failed),
        "cancelled" => Ok(SpdmAttestationStatus::Cancelled),
        "passed" => Ok(SpdmAttestationStatus::Passed),
        other => Err(DatabaseError::Internal {
            message: format!("Unexpected SPDM attestation status from DB: {other}"),
        }),
    }
}

pub async fn get_attestations_for_machine_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<Vec<SpdmDeviceAttestationDetails>, DatabaseError> {
    let query = r#"
        SELECT machine_id, device_id, ca_certificate, state, metadata, evidence, started_at, cancelled_at, completed_at
        FROM spdm_machine_devices_attestation
        WHERE machine_id = $1
    "#;

    let attestation_details: Vec<SpdmDeviceAttestationDetails> = sqlx::query_as(query)
        .bind(machine_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(attestation_details)
}

/// Returns the latest attestation details for each selected device on a machine.
///
/// Uses the same revision ordering as [`list_single_machine_attestation_status`].
/// Devices without records are omitted; an empty selection returns no details.
pub async fn get_latest_attestations_for_machine_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_ids: &[String],
) -> Result<Vec<SpdmDeviceAttestationDetails>, DatabaseError> {
    // Match the status aggregate so historical failures cannot appear as current failures.
    let query = r#"
        SELECT DISTINCT ON (machine_id, device_id)
            machine_id, device_id, ca_certificate, state, metadata, evidence, started_at, cancelled_at, completed_at
        FROM spdm_machine_devices_attestation
        WHERE machine_id = $1 AND device_id = ANY($2)
        ORDER BY machine_id, device_id, started_at DESC, job_id DESC
    "#;

    sqlx::query_as(query)
        .bind(machine_id)
        .bind(device_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Returns all device attestations for a job, or `NotFoundError` if it has no devices.
pub async fn get_attestations_for_job_id(
    txn: &mut PgConnection,
    job_id: &uuid::Uuid,
) -> Result<Vec<SpdmDeviceAttestationDetails>, DatabaseError> {
    let query = r#"
        SELECT machine_id, device_id, ca_certificate, state, metadata, evidence, started_at, cancelled_at, completed_at
        FROM spdm_machine_devices_attestation
        WHERE job_id = $1
    "#;

    let attestation_details: Vec<SpdmDeviceAttestationDetails> = sqlx::query_as(query)
        .bind(job_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // A job exists only through its device rows; an empty result means it is missing.
    if attestation_details.is_empty() {
        return Err(DatabaseError::NotFoundError {
            kind: "SPDM Attestation Job",
            id: job_id.to_string(),
        });
    }

    Ok(attestation_details)
}

/// `load_snapshot_for_machine_and_device_id` loads one device attestation with
/// its BMC connection data for the specified job.
///
/// A topology `bmc_info.machine_interface_id` selects the interface. Without
/// an ID, the query selects an attached BMC interface. If the named interface
/// no longer exists, is not attached to this machine, or is not a BMC
/// interface, the snapshot keeps its topology metadata without using a
/// different BMC connection.
pub async fn load_snapshot_for_machine_and_device_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_id: &String,
    job_id: &uuid::Uuid,
) -> Result<SpdmDeviceAttestation, DatabaseError> {
    let query = r#"
        SELECT
            mda.machine_id, mda.device_id, mda.nonce, mda.state,
            mda.metadata, mda.ca_certificate_link, mda.ca_certificate,
            mda.evidence_target, mda.evidence, mda.state_version,
            mda.state_outcome, mda.completed_at, mda.started_at, mda.cancelled_at,
            mda.job_id, mda.evidence_collection_locked,
            CASE WHEN mi.id IS NULL THEN
                COALESCE(mt.topology->'bmc_info', '{}'::jsonb) - 'ip' - 'mac'
            ELSE
                jsonb_strip_nulls(
                    COALESCE(mt.topology->'bmc_info', '{}'::jsonb) ||
                    jsonb_build_object(
                        'machine_interface_id', mi.id,
                        'ip', host(mia.address),
                        'mac', mi.mac_address::text
                    )
                )
            END AS bmc_info
        FROM spdm_machine_devices_attestation AS mda
        LEFT JOIN machine_topologies AS mt
            ON mt.machine_id = mda.machine_id
        LEFT JOIN machine_interfaces AS mi
            ON mi.machine_id = mda.machine_id
            AND mi.interface_type = 'Bmc'
            AND (
                mt.topology #>> '{bmc_info,machine_interface_id}' IS NULL
                OR mi.id::text = mt.topology #>> '{bmc_info,machine_interface_id}'
            )
        LEFT JOIN LATERAL (
            SELECT address
            FROM machine_interface_addresses
            WHERE interface_id = mi.id
            ORDER BY family(address), address
            LIMIT 1
        ) AS mia ON true
        WHERE mda.machine_id = $1
            AND mda.device_id = $2
            AND mda.job_id = $3;
    "#;

    let res: SpdmDeviceAttestation = sqlx::query_as(query)
        .bind(machine_id)
        .bind(device_id)
        .bind(job_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(res)
}

pub async fn list_machine_ids(txn: &mut PgConnection) -> Result<Vec<MachineId>, DatabaseError> {
    let query = r#"
        SELECT DISTINCT
            machine_id
        FROM 
            spdm_machine_devices_attestation
        WHERE
            completed_at is NULL
    "#;

    let res: Vec<MachineId> = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(res)
}

pub async fn persist_outcome(
    txn: &mut PgConnection,
    object_id: &SpdmObjectId,
    outcome: PersistentStateHandlerOutcome,
) -> Result<(), DatabaseError> {
    let query_device = r#"
        UPDATE 
            spdm_machine_devices_attestation
        SET state_outcome = $1
        WHERE machine_id = $2 AND device_id = $3 AND job_id = $4
    "#;

    sqlx::query(query_device)
        .bind(sqlx::types::Json(outcome))
        .bind(object_id.0)
        .bind(object_id.1.clone())
        .bind(object_id.2)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query_device, e))?
        .rows_affected();

    Ok(())
}

/// `persist_controller_state` writes the state and supplied replacement version
/// only when the device's state version matches `old_version`.
///
/// Returns `NotApplied(ControllerStateNotCurrent)` for a missing device or changed
/// version; database failures remain errors. An accepted write stores
/// `new_version` even when the state itself is unchanged. The caller owns the
/// transaction and its commit.
pub async fn persist_controller_state(
    txn: &mut PgConnection,
    object_id: &SpdmObjectId,
    old_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &SpdmAttestationState,
) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
    let query = r#"
            UPDATE 
                spdm_machine_devices_attestation
            SET state= $1, state_version=$2
            WHERE machine_id = $3 AND device_id = $4 AND job_id = $5 AND state_version = $6
        "#;
    let rows_affected = sqlx::query(query)
        .bind(sqlx::types::Json(new_state))
        .bind(new_version)
        .bind(object_id.0)
        .bind(object_id.1.clone())
        .bind(object_id.2)
        .bind(old_version)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .rows_affected();

    Ok(if rows_affected > 0 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    })
}

pub async fn update_history(
    txn: &mut PgConnection,
    object_id: &SpdmObjectId,
    state_snapshot: &SpdmAttestationState,
) -> Result<(), DatabaseError> {
    let query = r#"INSERT INTO spdm_device_attestation_history (machine_id, device_id, updated_at, state_snapshot)
    VALUES($1, $2, now(),  $3)
    "#;

    sqlx::query(query)
        .bind(object_id.0)
        .bind(object_id.1.clone())
        .bind(sqlx::types::Json(state_snapshot))
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, check_cases_async};
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    /// A requested device without evidence must not disappear from the machine's status.
    #[crate::sqlx_test]
    async fn selected_devices_without_results_are_in_progress(
        pool: sqlx::PgPool,
    ) -> eyre::Result<()> {
        #[derive(Debug, PartialEq)]
        enum StatusError {
            NotFound,
            Unexpected(String),
        }
        let populated_machine = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x71; 32],
            MachineType::Host,
        );
        let empty_machine = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x72; 32],
            MachineType::Host,
        );
        let mut txn = pool.begin().await?;
        for machine_id in [populated_machine, empty_machine] {
            sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
                .bind(machine_id)
                .execute(&mut *txn)
                .await?;
        }
        // Include an excluded failure so a complete selection proves the device filter still applies.
        let job_id = uuid::Uuid::new_v4();
        for (device_id, state) in [
            ("GPU_0", SpdmAttestationState::Passed),
            (
                "GPU_1",
                SpdmAttestationState::Failed("verification failed".to_string()),
            ),
        ] {
            sqlx::query(
                "INSERT INTO spdm_machine_devices_attestation
                 (machine_id, device_id, nonce, state, job_id, started_at)
                 VALUES ($1, $2, gen_random_uuid(), $3, $4, now())",
            )
            .bind(populated_machine)
            .bind(device_id)
            .bind(sqlx::types::Json(state))
            .bind(job_id)
            .execute(&mut *txn)
            .await?;
        }
        txn.commit().await?;

        check_cases_async([
            Case {
                scenario: "a missing selected device prevents existing passes from reporting passed",
                input: (populated_machine, Some(vec!["GPU_0".to_string(), "new-device".to_string()])),
                expect: Yields(SpdmAttestationStatus::InProgress),
            },
            Case {
                scenario: "missing selected work takes precedence over a stored failure",
                input: (populated_machine, Some(vec!["GPU_1".to_string(), "new-device".to_string()])),
                expect: Yields(SpdmAttestationStatus::InProgress),
            },
            Case {
                scenario: "a nonempty selection with no stored rows is in progress",
                input: (empty_machine, Some(vec!["new-device".to_string()])),
                expect: Yields(SpdmAttestationStatus::InProgress),
            },
            Case {
                scenario: "repeated IDs do not invent a missing device or include excluded failures",
                input: (populated_machine, Some(vec!["GPU_0".to_string(), "GPU_0".to_string()])),
                expect: Yields(SpdmAttestationStatus::Passed),
            },
            Case {
                scenario: "no explicit selection and no stored rows remains not found",
                input: (empty_machine, None),
                expect: FailsWith(StatusError::NotFound),
            },
            Case {
                scenario: "an empty explicit selection remains not found",
                input: (populated_machine, Some(Vec::new())),
                expect: FailsWith(StatusError::NotFound),
            },
        ], |(machine_id, device_ids)| {
            let pool = &pool;
            async move {
                let mut txn = pool.begin().await.expect("status transaction should start");
                list_single_machine_attestation_status(&mut txn, &machine_id, device_ids.as_deref())
                    .await
                    .map_err(|error| match error {
                        DatabaseError::NotFoundError { .. } => StatusError::NotFound,
                        other => StatusError::Unexpected(other.to_string()),
                    })
            }
        }).await;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn mixed_cancelled_attestation_statuses(pool: sqlx::PgPool) {
        struct Input {
            machine_seeds: &'static [u8],
            gpu_0_state: SpdmAttestationState,
            expected_status: SpdmAttestationStatus,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            job_status_matches: bool,
            machine_status_matches: bool,
            list_selectors_match: [bool; 4],
            predicted_machine_selectors_match: [bool; 4],
        }

        let cases = [
            Case {
                scenario: "passed and cancelled devices make a cancelled job",
                input: Input {
                    machine_seeds: &[1],
                    gpu_0_state: SpdmAttestationState::Passed,
                    expected_status: SpdmAttestationStatus::Cancelled,
                },
                expect: Yields(Observation {
                    job_status_matches: true,
                    machine_status_matches: true,
                    list_selectors_match: [true; 4],
                    predicted_machine_selectors_match: [true; 4],
                }),
            },
            Case {
                scenario: "failure takes precedence over cancellation",
                input: Input {
                    machine_seeds: &[2, 1],
                    gpu_0_state: SpdmAttestationState::Failed("verification failed".to_string()),
                    expected_status: SpdmAttestationStatus::Failed,
                },
                expect: Yields(Observation {
                    job_status_matches: true,
                    machine_status_matches: true,
                    list_selectors_match: [true; 4],
                    predicted_machine_selectors_match: [true; 4],
                }),
            },
            Case {
                scenario: "unfinished devices keep a cancelled job in progress",
                input: Input {
                    machine_seeds: &[3],
                    gpu_0_state: SpdmAttestationState::FetchMetadata,
                    expected_status: SpdmAttestationStatus::InProgress,
                },
                expect: Yields(Observation {
                    job_status_matches: true,
                    machine_status_matches: true,
                    list_selectors_match: [true; 4],
                    predicted_machine_selectors_match: [true; 4],
                }),
            },
        ];

        check_cases_async(cases, |input| {
            let pool = &pool;
            async move {
                let mut txn = pool.begin().await.map_err(|e| e.to_string())?;
                let mut machine_ids = input
                    .machine_seeds
                    .iter()
                    .map(|seed| {
                        MachineId::new(
                            MachineIdSource::ProductBoardChassisSerial,
                            [*seed; 32],
                            MachineType::Host,
                        )
                    })
                    .collect::<Vec<_>>();
                let machine_id = machine_ids[0];
                let job_id = uuid::Uuid::new_v4();

                // Insert members in fixture order to exercise sorting as well as device deduplication.
                for (index, member_id) in machine_ids.iter().enumerate() {
                    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
                        .bind(member_id)
                        .execute(&mut *txn)
                        .await
                        .map_err(|e| e.to_string())?;

                    // A passed second machine must be included even when another machine makes the job fail.
                    let devices = if index == 0 {
                        vec![
                            ("GPU_0", input.gpu_0_state.clone()),
                            ("GPU_1", SpdmAttestationState::Cancelled),
                        ]
                    } else {
                        vec![("GPU_0", SpdmAttestationState::Passed)]
                    };
                    for (device_id, state) in devices {
                        sqlx::query(
                            "INSERT INTO spdm_machine_devices_attestation \
                             (machine_id, device_id, nonce, state, job_id, started_at, triggered_by_mh_controller) \
                             VALUES ($1, $2, $3, $4, $5, now(), true)",
                        )
                        .bind(member_id)
                        .bind(device_id)
                        .bind(uuid::Uuid::new_v4())
                        .bind(sqlx::types::Json(state))
                        .bind(job_id)
                        .execute(&mut *txn)
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                }
                machine_ids.sort_by_key(ToString::to_string);

                let job_status = get_attestation_job_status(&mut txn, &job_id)
                    .await
                    .map_err(|e| e.to_string())?;
                let machine_status =
                    list_single_machine_attestation_status(&mut txn, &machine_id, None)
                        .await
                        .map_err(|e| e.to_string())?;
                let mut list_selectors_match = [false; 4];
                let mut predicted_machine_selectors_match = [false; 4];
                for (index, selector) in [
                    SpdmAttestationStatus::InProgress,
                    SpdmAttestationStatus::Failed,
                    SpdmAttestationStatus::Cancelled,
                    SpdmAttestationStatus::Passed,
                ]
                .into_iter()
                .enumerate()
                {
                    let statuses = list_all_attestation_statuses(&mut txn, selector)
                        .await
                        .map_err(|e| e.to_string())?;
                    let expected = if selector == input.expected_status {
                        vec![(job_id, input.expected_status, machine_ids.clone())]
                    } else {
                        Vec::new()
                    };
                    list_selectors_match[index] = statuses == expected;
                    // One job can span machines whose own aggregates differ.
                    let predicted = find_machine_attestation_statuses(&mut txn, None, Some(selector), 100)
                        .await.map_err(|error| error.to_string())?;
                    let expected = machine_ids.iter().copied().filter(|id| {
                        let status = if *id == machine_id {
                            input.expected_status
                        } else {
                            SpdmAttestationStatus::Passed
                        };
                        status == selector
                    }).collect::<Vec<_>>();
                    predicted_machine_selectors_match[index] = predicted.iter().all(|row| {
                        row.attestation_status == Some(selector)
                    }) && predicted.into_iter().map(|row| row.machine_id).collect::<Vec<_>>() == expected;
                }

                txn.rollback().await.map_err(|e| e.to_string())?;
                Ok::<_, String>(Observation {
                    job_status_matches: job_status == input.expected_status,
                    machine_status_matches: machine_status == input.expected_status,
                    list_selectors_match,
                    predicted_machine_selectors_match,
                })
            }
        })
        .await;
    }

    #[crate::sqlx_test]
    async fn machine_predictions_use_the_latest_controller_job(
        pool: sqlx::PgPool,
    ) -> eyre::Result<()> {
        let machines = [0x81, 0x82, 0x83, 0x84].map(|seed| {
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [seed; 32],
                MachineType::Host,
            )
        });
        let mut txn = pool.begin().await?;
        for id in machines {
            sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
                .bind(id)
                .execute(&mut *txn)
                .await?;
        }
        let older_job = uuid::Uuid::from_u128(1);
        let current_job = uuid::Uuid::from_u128(2);
        let external_job = uuid::Uuid::from_u128(3);
        let time = chrono::Utc::now();
        // Distinct boundaries: obsolete devices, external jobs, tied start times,
        // and both external-only and never-attested machines.
        for (machine, job, minutes, device, controller, state) in [
            (
                machines[0],
                older_job,
                -2,
                "removed",
                true,
                SpdmAttestationState::Failed("obsolete failure".to_string()),
            ),
            (
                machines[0],
                current_job,
                -1,
                "GPU_0",
                true,
                SpdmAttestationState::Passed,
            ),
            (
                machines[0],
                external_job,
                0,
                "GPU_0",
                false,
                SpdmAttestationState::Failed("external failure".to_string()),
            ),
            (
                machines[1],
                older_job,
                -1,
                "GPU_0",
                true,
                SpdmAttestationState::Passed,
            ),
            (
                machines[1],
                current_job,
                -1,
                "GPU_0",
                true,
                SpdmAttestationState::FetchMetadata,
            ),
            (
                machines[2],
                external_job,
                0,
                "GPU_0",
                false,
                SpdmAttestationState::Passed,
            ),
        ] {
            sqlx::query(
                "INSERT INTO spdm_machine_devices_attestation
                 (machine_id, device_id, nonce, state, job_id, started_at, triggered_by_mh_controller)
                 VALUES ($1, $2, gen_random_uuid(), $3, $4, $5, $6)",
            ).bind(machine).bind(device).bind(sqlx::types::Json(state)).bind(job)
                .bind(time + chrono::Duration::minutes(minutes)).bind(controller)
                .execute(&mut *txn).await?;
        }
        let mut expected = vec![
            (machines[0], Some(SpdmAttestationStatus::Passed)),
            (machines[1], Some(SpdmAttestationStatus::InProgress)),
            (machines[2], None),
            (machines[3], None),
        ];
        expected.sort_by_key(|row| row.0);
        let predictions = find_machine_attestation_statuses(&mut txn, None, None, 100).await?;
        assert_eq!(
            predictions
                .into_iter()
                .map(|row| (row.machine_id, row.attestation_status))
                .collect::<Vec<_>>(),
            expected
        );

        // Lookahead includes one extra row, and continuation excludes the cursor itself.
        for (scenario, cursor, expected_ids) in [
            ("first page plus lookahead", None, &expected[..2]),
            (
                "resume after the first ID",
                Some(expected[0].0),
                &expected[1..3],
            ),
            ("last page", Some(expected[2].0), &expected[3..]),
            ("past the last ID", Some(expected[3].0), &expected[4..]),
        ] {
            let predictions = find_machine_attestation_statuses(&mut txn, cursor, None, 1).await?;
            assert_eq!(
                predictions
                    .into_iter()
                    .map(|row| row.machine_id)
                    .collect::<Vec<_>>(),
                expected_ids.iter().map(|row| row.0).collect::<Vec<_>>(),
                "{scenario}"
            );
        }
        Ok(())
    }
}
