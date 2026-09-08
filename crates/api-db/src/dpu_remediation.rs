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
use std::ops::DerefMut;

use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::machine::{DpuMachineId, MachineId};
use model::dpu_remediation::{
    AppliedRemediation, ApproveRemediation, DisableRemediation, EnableRemediation,
    NewAppliedRemediation, NewRemediation, Remediation, RemediationApplicationStatus,
    RevokeRemediation,
};
use sqlx::Postgres;

use super::{ColumnInfo, FilterableQueryBuilder, ObjectColumnFilter};
use crate::{DatabaseError, DatabaseResult};

pub async fn persist_remediation(
    value: NewRemediation,
    txn: &mut sqlx::Transaction<'_, Postgres>,
) -> DatabaseResult<Remediation> {
    let (query, intermediate_query) = if let Some(metadata) = value.metadata.as_ref() {
        let query = "INSERT INTO dpu_remediations (metadata_name, metadata_description, metadata_labels, script, retries, script_author) VALUES ($1, $2, $3, $4, $5, $6) returning *";
        (
            query,
            sqlx::query_as(query)
                .bind(&metadata.name)
                .bind(&metadata.description)
                .bind(sqlx::types::Json(&metadata.labels)),
        )
    } else {
        let query = "INSERT INTO dpu_remediations (script, retries, script_author) VALUES ($1, $2, $3) returning *";
        (query, sqlx::query_as(query))
    };

    intermediate_query
        .bind(&value.script)
        .bind(value.retries)
        .bind(value.author.to_string())
        .fetch_one(txn.deref_mut())
        .await
        .map_err(|err| DatabaseError::new(query, err))
}

#[derive(Clone, Copy)]
pub struct RemediationIdColumn;
impl ColumnInfo<'_> for RemediationIdColumn {
    type TableType = Remediation;
    type ColumnType = RemediationId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Clone, Copy)]
pub struct EnabledColumn;

impl ColumnInfo<'_> for EnabledColumn {
    type TableType = Remediation;
    type ColumnType = bool;

    fn column_name(&self) -> &'static str {
        "enabled"
    }
}

pub async fn find_remediation_ids(
    txn: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<Vec<RemediationId>, DatabaseError> {
    let ids = find_remediations_by(txn, ObjectColumnFilter::<RemediationIdColumn>::All)
        .await?
        .into_iter()
        .map(|x| x.id)
        .collect();
    Ok(ids)
}

pub async fn find_remediations_by_ids(
    txn: &mut sqlx::Transaction<'_, Postgres>,
    remediation_ids: &[RemediationId],
) -> Result<Vec<Remediation>, DatabaseError> {
    let remediations = find_remediations_by(
        txn,
        ObjectColumnFilter::List(RemediationIdColumn, remediation_ids),
    )
    .await?;
    Ok(remediations)
}
pub async fn find_remediations_by<'a, C: ColumnInfo<'a, TableType = Remediation>>(
    txn: &mut sqlx::Transaction<'_, Postgres>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<Remediation>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new("SELECT * FROM dpu_remediations").filter(&filter);
    query
        .build_query_as()
        .fetch_all(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(query.sql(), e))
}

pub async fn find_next_remediation_for_machine(
    txn: &mut sqlx::Transaction<'_, Postgres>,
    machine_id: MachineId,
) -> Result<Option<Remediation>, DatabaseError> {
    for remediation in find_remediations_by(txn, ObjectColumnFilter::List(EnabledColumn, &[true]))
        .await?
        .into_iter()
    {
        let max_attempts = remediation.retries + 1;
        let remediations_applied =
            find_remediations_by_remediation_id_and_machine(txn, remediation.id, &machine_id)
                .await?;

        let attempted_so_far = remediations_applied.len() as i32;
        if attempted_so_far < max_attempts {
            if let Some(last_attempted) = remediations_applied.first()
                && last_attempted.succeeded
            {
                continue;
            }
            return Ok(Some(remediation));
        }
    }
    Ok(None)
}

pub async fn remediation_applied(
    txn: &mut sqlx::Transaction<'_, Postgres>,
    machine_id: MachineId,
    remediation_id: RemediationId,
    status: RemediationApplicationStatus,
) -> Result<(), DatabaseError> {
    let remediations_applied_so_far =
        find_remediations_by_remediation_id_and_machine(txn, remediation_id, &machine_id).await?;

    let attempt_for_this_remediation = match remediations_applied_so_far.first() {
        Some(last_applied_remediation) => last_applied_remediation.attempt + 1,
        None => 1,
    };
    let metadata = status.metadata.unwrap_or_default();

    let new_applied_remediation = NewAppliedRemediation {
        dpu_machine_id: machine_id.to_string(),
        id: remediation_id,
        succeeded: status.succeeded,
        status: metadata.labels,
        attempt: attempt_for_this_remediation,
    };

    let _ = persist_applied_remediation(new_applied_remediation, txn).await?;

    Ok(())
}

pub async fn persist_applied_remediation(
    value: NewAppliedRemediation,
    txn: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<AppliedRemediation, DatabaseError> {
    let query = "INSERT INTO applied_dpu_remediations (id, dpu_machine_id, attempt, succeeded, status) VALUES ($1, $2, $3, $4, $5) returning *";

    sqlx::query_as(query)
        .bind(value.id)
        .bind(&value.dpu_machine_id)
        .bind(value.attempt)
        .bind(value.succeeded)
        .bind(sqlx::types::Json(&value.status))
        .fetch_one(txn.deref_mut())
        .await
        .map_err(|err| DatabaseError::new(query, err))
}

#[derive(Clone, Copy)]
pub struct AppliedRemediationIdColumn;
impl ColumnInfo<'_> for AppliedRemediationIdColumn {
    type TableType = AppliedRemediation;
    type ColumnType = RemediationId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Clone, Copy)]
pub struct AppliedRemediationDpuMachineIdColumn;
impl ColumnInfo<'_> for AppliedRemediationDpuMachineIdColumn {
    type TableType = AppliedRemediation;
    type ColumnType = String;

    fn column_name(&self) -> &'static str {
        "dpu_machine_id"
    }
}

pub enum AppliedRemediationIdQueryType {
    Machine(DpuMachineId),
    RemediationId(RemediationId),
}

pub async fn find_applied_remediation_ids(
    txn: &mut sqlx::Transaction<'_, Postgres>,
    id_query_args: AppliedRemediationIdQueryType,
) -> Result<(Vec<RemediationId>, Vec<DpuMachineId>), DatabaseError> {
    let ids = match id_query_args {
        AppliedRemediationIdQueryType::Machine(machine_id) => {
            let remediation_ids = find_applied_remediations_by(
                txn,
                ObjectColumnFilter::List(
                    AppliedRemediationDpuMachineIdColumn,
                    &[machine_id.to_string()],
                ),
            )
            .await?
            .into_iter()
            .map(|x| x.id)
            .collect();

            (remediation_ids, vec![machine_id])
        }
        AppliedRemediationIdQueryType::RemediationId(remediation_id) => {
            let machine_ids = find_applied_remediations_by(
                txn,
                ObjectColumnFilter::List(AppliedRemediationIdColumn, &[remediation_id]),
            )
            .await?
            .into_iter()
            .map(|x| x.dpu_machine_id)
            .collect();

            (vec![remediation_id], machine_ids)
        }
    };

    Ok(ids)
}

pub async fn find_applied_remediations_by<'a, C: ColumnInfo<'a, TableType = AppliedRemediation>>(
    txn: &mut sqlx::Transaction<'_, Postgres>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<AppliedRemediation>, DatabaseError> {
    let mut query =
        FilterableQueryBuilder::new("SELECT * FROM applied_dpu_remediations").filter(&filter);
    query
        .build_query_as()
        .fetch_all(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(query.sql(), e))
}

// we cannot use the generic query for this one because we can't limit it to _two_ columns, unfortunately.
pub async fn find_remediations_by_remediation_id_and_machine(
    txn: &mut sqlx::Transaction<'_, Postgres>,
    remediation_id: RemediationId,
    machine_id: &MachineId,
) -> Result<Vec<AppliedRemediation>, DatabaseError> {
    let query = "SELECT * FROM applied_dpu_remediations WHERE id=$1 AND dpu_machine_id=$2 ORDER BY attempt DESC";
    sqlx::query_as(query)
        .bind(remediation_id)
        .bind(machine_id)
        .fetch_all(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(query, e))
}

pub async fn persist_approve_remediation(
    value: ApproveRemediation,
    txn: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), DatabaseError> {
    let existing_query = "SELECT * from dpu_remediations WHERE id=$1";
    let existing_remediation: Remediation = sqlx::query_as(existing_query)
        .bind(value.id)
        .fetch_optional(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(existing_query, e))?
        .ok_or(DatabaseError::NotFoundError {
            kind: "dpu_remediations.id",
            id: value.id.to_string(),
        })?;

    if existing_remediation.author.to_string().as_str() == value.reviewer.to_string().as_str() {
        return Err(DatabaseError::InvalidArgument("Reviewer cannot be the same person as Author for remediation, must be different person.".to_string()));
    } else if let Some(reviewer) = existing_remediation.reviewer.as_ref() {
        let reviewer = reviewer.to_string();
        if !reviewer.is_empty() {
            return Err(DatabaseError::InvalidArgument(format!(
                "Reviewer is already set to '{reviewer}', cannot overwrite.  Revoke if necessary.",
            )));
        }
    }

    let update_query = "UPDATE dpu_remediations SET script_reviewed_by=$1 WHERE id=$2";
    let _ = sqlx::query(update_query)
        .bind(value.reviewer.to_string())
        .bind(value.id)
        .execute(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(update_query, e))?;

    Ok(())
}

pub async fn persist_revoke_remediation(
    value: RevokeRemediation,
    txn: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), DatabaseError> {
    let update_query =
        "UPDATE dpu_remediations SET script_reviewed_by=NULL,enabled=false WHERE id=$1";
    let _ = sqlx::query(update_query)
        .bind(value.id)
        .execute(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(update_query, e))?;

    Ok(())
}

pub async fn persist_enable_remediation(
    value: EnableRemediation,
    txn: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), DatabaseError> {
    let existing_query = "SELECT * from dpu_remediations WHERE id=$1";
    let existing_remediation: Remediation = sqlx::query_as(existing_query)
        .bind(value.id)
        .fetch_optional(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(existing_query, e))?
        .ok_or(DatabaseError::NotFoundError {
            kind: "dpu_remediations.id",
            id: value.id.to_string(),
        })?;

    if existing_remediation.reviewer.is_none() {
        return Err(DatabaseError::InvalidArgument(
            "Cannot enable a remediation that has not been approved.".to_string(),
        ));
    }

    let update_query = "UPDATE dpu_remediations SET enabled=true WHERE id=$1";
    let _ = sqlx::query(update_query)
        .bind(value.id)
        .execute(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(update_query, e))?;

    Ok(())
}

pub async fn persist_disable_remediation(
    value: DisableRemediation,
    txn: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), DatabaseError> {
    let update_query = "UPDATE dpu_remediations SET enabled=false WHERE id=$1";
    let _ = sqlx::query(update_query)
        .bind(value.id)
        .execute(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new(update_query, e))?;

    Ok(())
}
