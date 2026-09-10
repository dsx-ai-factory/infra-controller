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

use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use config_version::ConfigVersion;
use model::vpc::{
    NewVpc, PowerResourceGroupUpdate, UpdateVpc, UpdateVpcVirtualization, Vpc, VpcStatus,
};
use sqlx::{PgConnection, PgTransaction};

use super::{ColumnInfo, FilterableQueryBuilder, ObjectColumnFilter, network_segment, vpc};
use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

#[derive(Clone, Copy)]
pub struct VniColumn;
impl ColumnInfo<'_> for crate::vpc::VniColumn {
    type TableType = Vpc;
    type ColumnType = i32;

    fn column_name(&self) -> &'static str {
        "vni"
    }
}

#[derive(Clone, Copy)]
pub struct IdColumn;
impl ColumnInfo<'_> for crate::vpc::IdColumn {
    type TableType = Vpc;
    type ColumnType = VpcId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Clone, Copy)]
pub struct NameColumn;
impl<'a> ColumnInfo<'a> for NameColumn {
    type TableType = Vpc;
    type ColumnType = &'a str;

    fn column_name(&self) -> &'static str {
        "name"
    }
}

pub async fn persist(
    value: NewVpc,
    status: VpcStatus,
    txn: &mut PgConnection,
) -> Result<Vpc, DatabaseError> {
    let query =
                "INSERT INTO vpcs (id, name, organization_id, network_security_group_id, version, network_virtualization_type,
                description,
                labels, routing_profile_type, routing_profile_overrides, vni, status, power_resource_group,
                slaac_enabled)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING *";
    sqlx::query_as(query)
        .bind(value.id)
        .bind(&value.metadata.name)
        .bind(&value.tenant_organization_id)
        .bind(&value.network_security_group_id)
        .bind(ConfigVersion::initial())
        .bind(value.network_virtualization_type)
        .bind(&value.metadata.description)
        .bind(sqlx::types::Json(&value.metadata.labels))
        .bind(value.routing_profile_type)
        .bind(value.routing_profile_overrides.map(sqlx::types::Json))
        .bind(value.vni)
        .bind(sqlx::types::Json(&status))
        .bind(value.power_resource_group)
        .bind(value.slaac_enabled)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::vpc::VpcSearchFilter,
) -> Result<Vec<VpcId>, DatabaseError> {
    // build query
    let mut builder = sqlx::QueryBuilder::new("SELECT id FROM vpcs WHERE ");
    let mut has_filter = false;
    if let Some(name) = &filter.name {
        builder.push("name = ");
        builder.push_bind(name);
        has_filter = true;
    }
    if let Some(tenant_org_id) = &filter.tenant_org_id {
        if has_filter {
            builder.push(" AND ");
        }
        builder.push("organization_id = ");
        builder.push_bind(tenant_org_id);
        has_filter = true;
    }
    if let Some(network_virtualization_type) = filter.network_virtualization_type {
        if has_filter {
            builder.push(" AND ");
        }
        builder.push("network_virtualization_type = ");
        builder.push_bind(network_virtualization_type);
        has_filter = true;
    }
    if let Some(label) = filter.label {
        if has_filter {
            builder.push(" AND ");
        }
        match (label.key.is_empty(), label.value) {
            // Label key is empty, label value is set.
            (true, Some(value)) => {
                builder.push(
                    " EXISTS (
                        SELECT 1
                        FROM jsonb_each_text(labels) AS kv
                        WHERE kv.value = ",
                );
                builder.push_bind(value);
                builder.push(")");
                has_filter = true;
            }
            // Label key is empty, label value is not set.
            (true, None) => {
                return Err(DatabaseError::InvalidArgument(
                    "finding VPCs based on label needs either key or a value.".to_string(),
                ));
            }
            // Label key is not empty, label value is not set.
            (false, None) => {
                builder.push(" labels ->> ");
                builder.push_bind(label.key);
                builder.push(" IS NOT NULL");
                has_filter = true;
            }
            // Label key is not empty, label value is set.
            (false, Some(value)) => {
                builder.push(" labels ->> ");
                builder.push_bind(label.key);
                builder.push(" = ");
                builder.push_bind(value);
                has_filter = true;
            }
        }
    }
    if has_filter {
        builder.push(" AND ");
    }
    builder.push("deleted IS NULL");

    let query = builder.build_query_as();
    let ids: Vec<VpcId> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("vpc::find_ids", e))?;

    Ok(ids)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VpcRowLock {
    #[default]
    None,
    /// Coordinates parent VPC mutations with VPC-attached child mutations.
    ///
    /// Callers that need this lock to protect later statements must use a
    /// connection from an explicit transaction and keep the transaction open until
    /// the coordinated mutation is committed or rolled back.
    Mutation,
}

// Note: Following find function should not be used to search based on vpc labels.
// Recommended approach to filter by labels is to first find VPC ids.
async fn find_by_inner<'a, C: ColumnInfo<'a, TableType = Vpc>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
    row_lock: VpcRowLock,
) -> Result<Vec<Vpc>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new("SELECT * FROM vpcs").filter(&filter);

    query.push(" AND deleted IS NULL");
    if matches!(row_lock, VpcRowLock::Mutation) {
        query.push(" FOR NO KEY UPDATE");
    }
    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

// Note: Following find function should not be used to search based on vpc labels.
// Recommended approach to filter by labels is to first find VPC ids.
pub async fn find_by_with_lock<'a, C: ColumnInfo<'a, TableType = Vpc>>(
    txn: &mut PgConnection,
    filter: ObjectColumnFilter<'a, C>,
    row_lock: VpcRowLock,
) -> Result<Vec<Vpc>, DatabaseError> {
    find_by_inner(&mut *txn, filter, row_lock).await
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = Vpc>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<Vpc>, DatabaseError> {
    find_by_inner(txn, filter, VpcRowLock::None).await
}

pub async fn find_by_vni(txn: &mut PgConnection, vni: i32) -> Result<Vec<Vpc>, DatabaseError> {
    let query = "SELECT * from vpcs WHERE (status->>'vni')::integer = $1 AND DELETED IS NULL";

    sqlx::query_as(query)
        .bind(vni)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Updates both persisted VNI locations for a VPC.
pub async fn set_vni(value: &Vpc, txn: &mut PgConnection, vni: i32) -> DatabaseResult<Vpc> {
    // Keep the requested VNI column and the actual status VNI in sync.
    let query = "UPDATE vpcs
            SET vni=$1, status=jsonb_set(status, '{vni}', to_jsonb($1::integer), true), updated=NOW()
            WHERE id=$2 AND deleted is null
            RETURNING *";
    sqlx::query_as(query)
        .bind(vni)
        .bind(value.id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_by_name(txn: impl DbReader<'_>, name: &str) -> Result<Vec<Vpc>, DatabaseError> {
    find_by(txn, ObjectColumnFilter::One(NameColumn, &name)).await
}

/// Finds the VPC attached to `segment_id`, including a soft-deleted VPC while
/// its segment is retained for controller cleanup.
pub async fn find_by_segment(
    txn: impl DbReader<'_>,
    segment_id: NetworkSegmentId,
) -> Result<Option<Vpc>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new(
        "SELECT v.* from vpcs v INNER JOIN network_segments s ON v.id = s.vpc_id",
    )
    .filter_relation(
        &ObjectColumnFilter::One(network_segment::IdColumn, &segment_id),
        Some("s"),
    );
    query.push(" LIMIT 1");

    query
        .build_query_as()
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

/// Tries to delete a VPC.
///
/// If the VPC existed at the point of deletion, this returns the last known information about the VPC.
/// If the VPC was already deleted, this returns Ok(`None`), even if historical
/// records still reference it.
///
/// Callers that coordinate VPC-attached child mutations must acquire
/// [`VpcRowLock::Mutation`] on this VPC before calling this function.
pub async fn try_delete(txn: &mut PgConnection, id: VpcId) -> Result<Option<Vpc>, DatabaseError> {
    let active_vpc_query = "SELECT EXISTS(SELECT 1 FROM vpcs WHERE id=$1 AND deleted IS NULL)";
    let is_active: bool = sqlx::query_scalar(active_vpc_query)
        .bind(id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(active_vpc_query, e))?;
    if !is_active {
        return Ok(None);
    }

    // Block deletion while any active or soft-deleted prefix row still
    // references this active VPC.
    let vpc_prefix_count_query = "SELECT count(*) FROM network_vpc_prefixes WHERE vpc_id=$1";
    let vpc_prefix_count: i64 = sqlx::query_scalar(vpc_prefix_count_query)
        .bind(id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(vpc_prefix_count_query, e))?;
    if vpc_prefix_count > 0 {
        return Err(DatabaseError::FailedPrecondition(format!(
            "VPC {id} cannot be deleted while {vpc_prefix_count} VPC prefixes still exist or are pending deletion"
        )));
    }

    // No need to check "deleted IS NULL" because there are no "legacy" cases (this field was
    // introduced at the same time as this check: deleted will not be set unless there are no
    // instance_addresses in the first place.)
    let instance_address_count_query = "SELECT count(*) FROM instance_addresses WHERE vpc_id=$1";
    let instance_address_count: i64 = sqlx::query_scalar(instance_address_count_query)
        .bind(id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(instance_address_count_query, e))?;
    if instance_address_count > 0 {
        return Err(DatabaseError::FailedPrecondition(format!(
            "VPC {id} cannot be deleted while {instance_address_count} instance addresses still reference it"
        )));
    }

    let instance_config_reference_count = crate::instance::count_vpc_references(txn, &id).await?;
    if instance_config_reference_count > 0 {
        return Err(DatabaseError::FailedPrecondition(format!(
            "VPC {id} cannot be deleted while {instance_config_reference_count} instance network configurations still reference it"
        )));
    }

    let query =
        "UPDATE vpcs SET updated=NOW(), deleted=NOW() WHERE id=$1 AND deleted IS NULL RETURNING *";
    match sqlx::query_as(query).bind(id).fetch_one(txn).await {
        Ok(vpc) => Ok(Some(vpc)),
        Err(sqlx::Error::RowNotFound) => Ok(None),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

pub async fn update(value: &UpdateVpc, txn: &mut PgConnection) -> DatabaseResult<Vpc> {
    // TODO: Should this check for deletion?
    let current_version = match value.if_version_match {
        Some(version) => version,
        None => {
            let vpcs =
                find_by(&mut *txn, ObjectColumnFilter::One(vpc::IdColumn, &value.id)).await?;
            if vpcs.len() != 1 {
                return Err(DatabaseError::FindOneReturnedManyResultsError(
                    value.id.into(),
                ));
            }
            vpcs[0].version
        }
    };
    let next_version = current_version.increment();

    // An omitted override preserves the current definition. A present message
    // replaces it so its unset properties inherit from the named base profile.
    // An omitted power resource group operation preserves the current value.
    // Set replaces it and clear stores NULL.
    // network_virtualization_type cannot be changed currently
    // TODO check number of changed rows
    let query = "UPDATE vpcs
            SET name=$1, version=$2, description=$3, network_security_group_id=$4,
                labels=$5::json,
                routing_profile_overrides=COALESCE($6::jsonb, routing_profile_overrides),
                power_resource_group=CASE WHEN $7 THEN $8 ELSE power_resource_group END,
                updated=NOW()
            WHERE id=$9 AND version=$10 AND deleted is null
            RETURNING *";
    let (update_power_resource_group, power_resource_group) =
        match value.power_resource_group.as_ref() {
            Some(PowerResourceGroupUpdate::Set(resource_group)) => {
                (true, Some(resource_group.as_str()))
            }
            Some(PowerResourceGroupUpdate::Clear) => (true, None),
            None => (false, None),
        };
    let query_result = sqlx::query_as(query)
        .bind(&value.metadata.name)
        .bind(next_version)
        .bind(&value.metadata.description)
        .bind(&value.network_security_group_id)
        .bind(sqlx::types::Json(&value.metadata.labels))
        .bind(
            value
                .routing_profile_overrides
                .as_ref()
                .map(sqlx::types::Json),
        )
        .bind(update_power_resource_group)
        .bind(power_resource_group)
        .bind(value.id)
        .bind(current_version)
        .fetch_one(txn)
        .await;

    match query_result {
        Ok(r) => Ok(r),
        Err(sqlx::Error::RowNotFound) => {
            // TODO: This can actually happen on both invalid ID and invalid version
            // So maybe this should be `ObjectNotFoundOrModifiedError`
            Err(DatabaseError::ConcurrentModificationError(
                "vpc",
                current_version.to_string(),
            ))
        }
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

pub async fn update_virtualization(
    value: &UpdateVpcVirtualization,
    // Note: This is a PgTransaction, not a PgConnection, because we will be doing table locking,
    // which must happen in a transaction.
    txn: &mut PgTransaction<'_>,
) -> DatabaseResult<Vpc> {
    let query = "UPDATE vpcs
            SET version=$1, network_virtualization_type=$2, updated=NOW()
            WHERE id=$3 AND version=$4 AND deleted is null
            RETURNING *";

    let current_version = match value.if_version_match {
        Some(version) => version,
        None => {
            let vpcs = find_by(
                txn.as_mut(),
                ObjectColumnFilter::One(vpc::IdColumn, &value.id),
            )
            .await?;
            if vpcs.len() != 1 {
                return Err(DatabaseError::FindOneReturnedManyResultsError(
                    value.id.into(),
                ));
            }
            vpcs[0].version
        }
    };
    let next_version = current_version.increment();

    let query_result = sqlx::query_as(query)
        .bind(next_version)
        .bind(value.network_virtualization_type)
        .bind(value.id)
        .bind(current_version)
        .fetch_one(txn.deref_mut())
        .await;

    let vpc: Vpc = match query_result {
        Ok(r) => Ok(r),
        Err(sqlx::Error::RowNotFound) => {
            // TODO(chet): This can actually happen on both invalid ID and invalid
            // version, so maybe this should be `ObjectNotFoundOrModifiedError`
            // or similar.
            Err(DatabaseError::ConcurrentModificationError(
                "vpc",
                current_version.to_string(),
            ))
        }
        Err(e) => Err(DatabaseError::query(query, e)),
    }?;

    // Update SVI IP for stretchable segments.
    let network_segments = crate::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(network_segment::VpcColumn, &vpc.id),
        model::network_segment::NetworkSegmentSearchConfig::default(),
    )
    .await?;

    for network_segment in network_segments {
        if !network_segment.status.can_stretch.unwrap_or_default() {
            continue;
        }

        if network_segment.prefixes.is_empty()
            || network_segment
                .prefixes
                .iter()
                .any(|prefix| prefix.svi_ip.is_none())
        {
            // If we can't update SVI IP in any of these segment, we have to fail whole operation.
            crate::network_segment::allocate_svi_ip(&network_segment, txn).await?;
        }
    }

    Ok(vpc)
}

// Increments the VPC version field. This is used when modifying resources that
// are attached to this VPC but are not directly part of the `vpcs` table (e.g.
// VPC prefixes).
pub async fn increment_vpc_version(
    txn: &mut PgConnection,
    id: VpcId,
    expected_version: ConfigVersion,
) -> Result<ConfigVersion, DatabaseError> {
    let next_version = expected_version.increment();

    let update_query = "UPDATE vpcs SET version = $1 WHERE id = $2 AND version = $3 AND deleted IS NULL RETURNING version";
    let updated: Result<(ConfigVersion,), _> = sqlx::query_as(update_query)
        .bind(next_version)
        .bind(id)
        .bind(expected_version)
        .fetch_one(&mut *txn)
        .await;

    match updated {
        Ok((version,)) => Ok(version),
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::ConcurrentModificationError(
            "vpc",
            expected_version.to_string(),
        )),
        Err(e) => Err(DatabaseError::query(update_query, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repeating deletion preserves its `Ok(None)` contract even if retained
    /// instance JSON still references the historical VPC.
    #[crate::sqlx_test]
    async fn try_delete_ignores_references_to_already_deleted_vpc(pool: sqlx::PgPool) {
        let vpc_id = VpcId::new();
        let machine_id = uuid::Uuid::new_v4();
        let mut txn = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO vpcs (id, name, version, deleted) \
             VALUES ($1, 'already-deleted-vpc', 'V1-T0', NOW())",
        )
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO instances (machine_id, network_config) \
             VALUES ($1, jsonb_build_object( \
                 'interfaces', jsonb_build_array(jsonb_build_object('vpc_id', $2::text))))",
        )
        .bind(machine_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        assert!(try_delete(txn.as_mut(), vpc_id).await.unwrap().is_none());
        txn.rollback().await.unwrap();
    }
}
