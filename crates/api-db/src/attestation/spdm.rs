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

pub async fn insert_device_attestations(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    devices: Vec<SpdmDeviceAttestation>,
) -> DatabaseResult<u64> {
    let query = "DELETE FROM spdm_machine_devices_attestation WHERE machine_id=$1";
    sqlx::query(query)
        .bind(machine_id)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

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

    let query = r#"INSERT INTO spdm_machine_devices_attestation (machine_id, device_id, nonce, state, state_version, ca_certificate_link, evidence_target, started_at)
        SELECT 
            $1 as machine_id, device_id, nonce, state, state_version, ca_certificate_link, evidence_target, started_at
        FROM 
            UNNEST($2::TEXT[], $3::uuid[], $4::JSONB[], $5::TEXT[], $6::TEXT[], $7::TEXT[], $8::timestamptz[])
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
        .bind(started_at_timestamps)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(res.rows_affected())
}

pub async fn cancel_machine_attestation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> DatabaseResult<()> {
    let current_time = chrono::Utc::now();
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET cancelled_at = $2
        WHERE machine_id = $1
        "#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(current_time)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn set_completed_at(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_id: &str,
) -> DatabaseResult<()> {
    let current_time = chrono::Utc::now();
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET completed_at = $3
        WHERE machine_id = $1 and device_id = $2
        "#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
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
    metadata: &SpdmMachineDeviceMetadata,
) -> DatabaseResult<()> {
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET metadata = $3
        WHERE machine_id = $1 AND device_id = $2"#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
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
    certificate: &CaCertificate,
) -> DatabaseResult<()> {
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET ca_certificate = $3
        WHERE machine_id = $1 AND device_id = $2"#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
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
    evidence: &Evidence,
) -> DatabaseResult<()> {
    let query = r#"UPDATE spdm_machine_devices_attestation
        SET evidence = $3
        WHERE machine_id = $1 AND device_id = $2"#;
    sqlx::query(query)
        .bind(machine_id)
        .bind(device_id)
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
            machine_id, device_id
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

pub enum ListSelector {
    None,
    Unsuccessful,
    InProgress,
}

// list_all_attestation_statuses (selector)
pub async fn list_all_attestation_statuses(
    txn: &mut PgConnection,
    selector: ListSelector,
) -> Result<Vec<(MachineId, SpdmAttestationStatus)>, DatabaseError> {
    let query = match selector {
        ListSelector::None => {
            r#"
        WITH machine_attestation_statuses AS (
            SELECT
                machine_id,
                CASE
                    WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                        THEN 'in_progress'
                    WHEN bool_or(state ? 'Failed')
                        THEN 'failed'
                    WHEN bool_and(state = '"Cancelled"'::jsonb)
                        THEN 'cancelled'
                    WHEN bool_and(state = '"Passed"'::jsonb)
                        THEN 'passed'
                END AS attestation_status
            FROM spdm_machine_devices_attestation
            GROUP BY machine_id
        )
        SELECT
            machine_id,
            attestation_status
        FROM machine_attestation_statuses;
            "#
        }
        ListSelector::Unsuccessful => {
            r#"
        WITH machine_attestation_statuses AS (
            SELECT
                machine_id,
                CASE
                    WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                        THEN 'in_progress'
                    WHEN bool_or(state ? 'Failed')
                        THEN 'failed'
                    WHEN bool_and(state = '"Cancelled"'::jsonb)
                        THEN 'cancelled'
                    WHEN bool_and(state = '"Passed"'::jsonb)
                        THEN 'passed'
                END AS attestation_status
            FROM spdm_machine_devices_attestation
            GROUP BY machine_id
        )
        SELECT
            machine_id,
            attestation_status
        FROM machine_attestation_statuses
        WHERE attestation_status = 'failed';
        "#
        }
        ListSelector::InProgress => {
            r#"
        WITH machine_attestation_statuses AS (
            SELECT
                machine_id,
                CASE
                    WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                        THEN 'in_progress'
                    WHEN bool_or(state ? 'Failed')
                        THEN 'failed'
                    WHEN bool_and(state = '"Cancelled"'::jsonb)
                        THEN 'cancelled'
                    WHEN bool_and(state = '"Passed"'::jsonb)
                        THEN 'passed'
                END AS attestation_status
            FROM spdm_machine_devices_attestation
            GROUP BY machine_id
        )
        SELECT
            machine_id,
            attestation_status
        FROM machine_attestation_statuses
        WHERE attestation_status = 'in_progress';
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
            let machine_id: MachineId = elem
                .try_get("machine_id")
                .map_err(|e| DatabaseError::new(query, e))?;
            let status: String = elem
                .try_get("attestation_status")
                .map_err(|e| DatabaseError::new(query, e))?;

            Ok((
                machine_id,
                convert_status_str_to_model_status(status.as_str())?,
            ))
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;

    Ok(attestation_statuses)
}

pub async fn list_single_machine_attestation_status(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<SpdmAttestationStatus, DatabaseError> {
    let query = r#"
        SELECT
            CASE
                WHEN bool_or(state NOT IN ('"Passed"'::jsonb, '"Cancelled"'::jsonb) AND NOT (state ? 'Failed'))
                    THEN 'in_progress'
                WHEN bool_or(state ? 'Failed')
                    THEN 'failed'
                WHEN bool_and(state = '"Cancelled"'::jsonb)
                    THEN 'cancelled'
                WHEN bool_and(state = '"Passed"'::jsonb)
                    THEN 'passed'
            END AS attestation_status
        FROM spdm_machine_devices_attestation
        WHERE machine_id = $1
        HAVING count(*) > 0
    "#;

    let status = sqlx::query_scalar::<_, String>(query)
        .bind(machine_id)
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

/// this loads a device attestation from the DB
pub async fn load_snapshot_for_machine_and_device_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    device_id: &String,
) -> Result<SpdmDeviceAttestation, DatabaseError> {
    let query = r#"
        SELECT
            mda.*,
            jsonb_strip_nulls(
                COALESCE(mt.topology->'bmc_info', '{}'::jsonb) ||
                jsonb_build_object(
                    'machine_interface_id', mi.id,
                    'ip', host(mia.address),
                    'mac', mi.mac_address::text
                )
            ) AS bmc_info
        FROM spdm_machine_devices_attestation AS mda
        LEFT JOIN machine_topologies AS mt
            ON mt.machine_id = mda.machine_id
        JOIN machine_interfaces AS mi
            ON mi.machine_id = mda.machine_id
            AND mi.interface_type = 'Bmc'
        LEFT JOIN LATERAL (
            SELECT address
            FROM machine_interface_addresses
            WHERE interface_id = mi.id
            ORDER BY family(address), address
            LIMIT 1
        ) AS mia ON true
        WHERE mda.machine_id = $1
            AND mda.device_id  = $2;
    "#;

    let res: SpdmDeviceAttestation = sqlx::query_as(query)
        .bind(machine_id)
        .bind(device_id)
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
        WHERE machine_id = $2 AND device_id = $3
    "#;

    sqlx::query(query_device)
        .bind(sqlx::types::Json(outcome))
        .bind(object_id.0)
        .bind(object_id.1.clone())
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
            WHERE machine_id = $3 AND device_id = $4 AND state_version = $5
        "#;
    let rows_affected = sqlx::query(query)
        .bind(sqlx::types::Json(new_state))
        .bind(new_version)
        .bind(object_id.0)
        .bind(object_id.1.clone())
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
