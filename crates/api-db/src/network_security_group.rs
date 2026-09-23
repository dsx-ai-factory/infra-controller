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
use carbide_uuid::instance::InstanceId;
use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use carbide_uuid::vpc::VpcId;
use config_version::ConfigVersion;
use model::metadata::Metadata;
use model::network_security_group::{
    NetworkSecurityGroup, NetworkSecurityGroupAttachments,
    NetworkSecurityGroupPropagationObjectStatus, NetworkSecurityGroupRule,
};
use model::tenant::TenantOrganizationId;
use sqlx::{PgConnection, Postgres};

use crate::DatabaseError;

/// Creates a new NetworkSecurityGroup DB record.  It enforces a unique `name` by
/// only creating if there is no active record found with the same name.
///
/// * `txn`                    - A reference to an active DB transaction
/// * `id`                     - A reference to a NetworkSecurityGroupId to be set as
///   the id for the new NetworkSecurityGroup
/// * `tenant_organization_id` - A reference to a TenantOrganizationId containing the
///   tenant org that owns this NetworkSecurityGroup
/// * `created_by`             - Optional String containing an ID to track the user who
///   created the NetworkSecurityGroup
/// * `metadata`               - A reference to a Metadata struct containing extra
///   details about the NetworkSecurityGroup
/// * `stateful_egress`        - Whether egress rules are stateful.
/// * `rules`                  - A slice of NetworkSecurityGroupRule containing the ACLs
///   of the NetworkSecurityGroup
pub async fn create(
    txn: &mut PgConnection,
    id: &NetworkSecurityGroupId,
    tenant_organization_id: &TenantOrganizationId,
    created_by: Option<&str>,
    metadata: &Metadata,
    stateful_egress: bool,
    rules: &[NetworkSecurityGroupRule],
) -> Result<NetworkSecurityGroup, DatabaseError> {
    let query = "INSERT INTO network_security_groups
                (id, tenant_organization_id, name, labels, description, rules, version, created_by, stateful_egress)
            SELECT $1::varchar, $2::varchar, $3::varchar, $4::jsonb, $5::varchar, $6::jsonb, $7::varchar, $8::varchar, $9
            WHERE NOT EXISTS
                /* There should be a unique constraint on id.  The condition here is just defensive. */
                (SELECT id FROM network_security_groups WHERE (id=$1::varchar OR (name=$3::varchar AND tenant_organization_id=$2::varchar)) AND deleted IS NULL)
            RETURNING *";

    match sqlx::query_as::<Postgres, NetworkSecurityGroup>(query)
        .bind(id)
        .bind(tenant_organization_id.to_string())
        .bind(&metadata.name)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(&metadata.description)
        .bind(sqlx::types::Json(rules))
        .bind(ConfigVersion::initial())
        .bind(created_by)
        .bind(stateful_egress)
        .fetch_one(txn)
        .await
    {
        Ok(network_security_group) => Ok(network_security_group),
        // This error should only show up when we didn't
        // create a record because the subquery found an existing name already.
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::AlreadyFoundError {
            kind: "NetworkSecurityGroup",
            id: metadata.name.clone(),
        }),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

/// Returns a list of IDs for all non-deleted NetworkSecurityGroup records.
///
/// * `txn`                    - A reference to an active DB transaction
/// * `name`                   - Optional String containing the name of a desired
///   NetworkSecurityGroup
/// * `tenant_organization_id` - Optional TenantOrganizationId containing the tenant
///   org to match against NetworkSecurityGroup records.
/// * `for_update`             - A boolean flag to acquire DB locks for
///   synchronization
pub async fn find_ids(
    txn: &mut PgConnection,
    name: Option<&str>,
    tenant_organization_id: Option<&TenantOrganizationId>,
    for_update: bool,
) -> Result<Vec<NetworkSecurityGroupId>, DatabaseError> {
    let mut builder =
        sqlx::QueryBuilder::new("SELECT id FROM network_security_groups WHERE deleted is NULL");

    if name.is_some() {
        builder.push(" AND name = ");
        builder.push_bind(name);
    }

    if tenant_organization_id.is_some() {
        builder.push(" AND tenant_organization_id = ");
        builder.push_bind(tenant_organization_id.map(|t| t.to_string()));
    }

    if for_update {
        builder.push(" ORDER BY id ");
        builder.push(" FOR UPDATE ");
    }

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(builder.sql(), err))
}

/// Queries the DB for non-deleted NetworkSecurityGroup records
/// based on the supplied list of IDs
///
/// * `txn`                        - A reference to an active DB transaction
/// * `network_security_group_ids` - A list of NetworkSecurityGroupId values to use for
///   querying the Db for active NetworkSecurityGroup records
/// * `tenant_organization_id`     - Optional reference to TenantOrganizationId containing the
///   tenant org to match against NetworkSecurityGroup records.
/// * `for_update`                 - A boolean flag to acquire DB locks for synchronization
pub async fn find_by_ids(
    txn: &mut PgConnection,
    network_security_group_ids: &[NetworkSecurityGroupId],
    tenant_organization_id: Option<&TenantOrganizationId>,
    for_update: bool,
) -> Result<Vec<NetworkSecurityGroup>, DatabaseError> {
    let mut builder =
        sqlx::QueryBuilder::new("SELECT * from network_security_groups WHERE deleted is NULL");

    builder.push(" AND id = ANY(");
    builder.push_bind(network_security_group_ids);
    builder.push(") ");

    if tenant_organization_id.is_some() {
        builder.push(" AND tenant_organization_id = ");
        builder.push_bind(tenant_organization_id.map(|t| t.to_string()));
    }

    if for_update {
        builder.push(" ORDER BY id ");
        builder.push(" FOR UPDATE ");
    }

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(builder.sql(), err))
}

/// Queries the DB for objects that have attached NetworkSecurityGroups
///
/// * `txn`                        - A reference to an active DB transaction
/// * `network_security_group_ids` - A list of NetworkSecurityGroupId values to use for
///   querying the Db for active NetworkSecurityGroup records
/// * `tenant_organization_id`     - Optional TenantOrganizationId containing the tenant org to
///   match against NetworkSecurityGroup records.
///
/// ***NOTE:  No locking is guaranteed.***
/// The caller is responsible for coordination, most likely with row-level locks on
/// NetworkSecurityGroup; otherwise, it's possible for something to update an object
/// (VPC/Instance/etc) to attach a security group after this function has already returned
/// its results. This may or may not be acceptable depending on the use-case.
///          
pub async fn find_objects_with_attachments(
    txn: &mut PgConnection,
    network_security_group_ids: Option<&[NetworkSecurityGroupId]>,
    tenant_organization_id: Option<&TenantOrganizationId>,
) -> Result<Vec<NetworkSecurityGroupAttachments>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT
                  nsg.id as id,
                  COALESCE(JSON_AGG(v.id) FILTER (WHERE v.id IS NOT NULL), '[]') as vpc_ids,
                  COALESCE(JSON_AGG(i.id) FILTER (WHERE i.id IS NOT NULL), '[]') as instance_ids
              FROM
                  network_security_groups nsg
              LEFT OUTER JOIN vpcs v ON v.network_security_group_id=nsg.id AND v.deleted IS NULL
              LEFT OUTER JOIN instances i ON i.network_security_group_id=nsg.id AND i.deleted IS NULL
              WHERE
                nsg.deleted IS NULL",
    );

    if network_security_group_ids.is_some() {
        builder.push(" AND nsg.id = ANY(");
        builder.push_bind(network_security_group_ids);
        builder.push(") ");
    }

    if tenant_organization_id.is_some() {
        builder.push(" AND nsg.tenant_organization_id = ");
        builder.push_bind(tenant_organization_id.map(|t| t.to_string()));
    }

    builder.push(" GROUP BY nsg.id ");

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(builder.sql(), err))
}

/// `find_retained_attachments` finds resources whose configuration still uses
/// this NSG, including resources being deleted. Deletion intent does not mean
/// the DPU has stopped serving their tenant network.
///
/// The caller holds the NSG row lock while inspecting and changing its policy.
pub async fn find_retained_attachments(
    txn: &mut PgConnection,
    id: &NetworkSecurityGroupId,
) -> Result<NetworkSecurityGroupAttachments, DatabaseError> {
    let query = "SELECT $1::varchar AS id,
        COALESCE((SELECT json_agg(id) FROM vpcs
                  WHERE network_security_group_id = $1), '[]') AS vpc_ids,
        COALESCE((SELECT json_agg(id) FROM instances
                  WHERE network_security_group_id = $1), '[]') AS instance_ids";
    sqlx::query_as(query)
        .bind(id)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Queries the DB for the NSG propagation status across sets of objects
///
/// * `txn`                        - A reference to an active DB transaction
/// * `network_security_group_ids` - A list of NetworkSecurityGroupId values to use for
///   querying the Db for active NetworkSecurityGroup records
/// * `tenant_organization_id`     - Optional reference to a TenantOrganizationId for the
///   tenant org to match against NetworkSecurityGroup records.
/// * `vpc_ids`                    - Optional list of VpcId to query for propagation status
/// * `instance_ids`               - Optional list of InstanceId to query for propagation status
///          
pub async fn get_propagation_status(
    txn: &mut PgConnection,
    network_security_group_ids: Option<&[NetworkSecurityGroupId]>,
    tenant_organization_id: Option<&TenantOrganizationId>,
    vpc_ids: Option<&[VpcId]>,
    instance_ids: Option<&[InstanceId]>,
) -> Result<
    (
        Vec<NetworkSecurityGroupPropagationObjectStatus>,
        Vec<NetworkSecurityGroupPropagationObjectStatus>,
    ),
    DatabaseError,
> {
    let mut vpc_query_builder = sqlx::QueryBuilder::new(
        // Querying for VPC status is slightly more complicated because
        // instance records don't have a vpc_id column, so we need to
        // start on instances and resolve the VPC through each persisted
        // interface. Prefer its resolved vpc_id and fall back to the
        // attached segment for legacy configurations. Addressless SLAAC
        // interfaces deliberately have no instance_addresses row.
        // This does seem like it might have the upside of accounting for
        // a future case where a machine has multiple DPUs within separate
        // VPCs.
        "
        SELECT
        vpc_id::text as id,
        sum(interfaces_expected)::INT4 as interfaces_expected,
        sum(interfaces_applied)::INT4 as interfaces_applied,
        COALESCE(json_agg(distinct instance_id) FILTER (WHERE interfaces_expected != interfaces_applied), '[]') as unpropagated_instance_ids,
        COALESCE(json_agg(distinct instance_id) FILTER (WHERE instance_id IS NOT  NULL), '[]') as related_instance_ids
        FROM (
            SELECT
                v.id as vpc_id, i.id as instance_id,
                /*
                * Get the number of interfaces associated with the instance
                * that do not have NSGs on the interface.
                */
                count(distinct(ifc->'internal_uuid'))::int as interfaces_expected,
                /*
                * Get the count of interfaces where the source is VPC
                * and the NSG ID and version matches those of the VPC.
                */
                coalesce(sum(
                        (ifco #> '{network_security_group}'->>'source' = 'VPC'
                        AND
                        ifco #> '{network_security_group}'->>'id' = v.network_security_group_id
                        AND
                        ifco #> '{network_security_group}'->>'version' = nsg.version)::int
                ), 0)::int as interfaces_applied
            FROM
                instances i
            JOIN jsonb_array_elements(i.network_config #>'{interfaces}') ifc on ifc->>'network_security_group_id' IS NULL
            JOIN machine_interfaces mi ON mi.machine_id = i.machine_id
            JOIN machines dpu ON dpu.id = mi.attached_dpu_machine_id
            /* network_status_observation is stored in dpu now. */
            LEFT OUTER JOIN jsonb_array_elements(dpu.network_status_observation #>'{instance_network_observation,interfaces}') ifco on ifco->>'internal_uuid' = ifc->>'internal_uuid'
            JOIN vpcs v ON v.id = COALESCE(
                (ifc->>'vpc_id')::uuid,
                (
                    SELECT network_segments.vpc_id
                    FROM network_segments
                    WHERE network_segments.id = (ifc->>'network_segment_id')::uuid
                )
            )
            JOIN network_security_groups nsg on nsg.id=v.network_security_group_id
            WHERE i.network_security_group_id IS NULL
            AND i.deleted IS NULL"
    );

    if network_security_group_ids.is_some() {
        vpc_query_builder.push(" AND nsg.id = ANY(");
        vpc_query_builder.push_bind(network_security_group_ids);
        vpc_query_builder.push(") ");
    }

    if vpc_ids.is_some() {
        vpc_query_builder.push(" AND v.id = ANY(");
        vpc_query_builder.push_bind(vpc_ids);
        vpc_query_builder.push(") ");
    }

    if tenant_organization_id.is_some() {
        vpc_query_builder.push(" AND nsg.tenant_organization_id = ");
        vpc_query_builder.push_bind(tenant_organization_id.map(|t| t.to_string()));
    }

    vpc_query_builder.push(" GROUP BY v.id, i.id) as prop_stats GROUP BY vpc_id");

    let mut instance_query_builder = sqlx::QueryBuilder::new("
        SELECT
        instance_id::text as id,
        interfaces_expected,
        interfaces_applied,

        /* Provide a list of instances related to the object that don't have the correct NSG details. */    
        COALESCE(json_agg(distinct instance_id) FILTER (WHERE interfaces_expected != interfaces_applied), '[]') as unpropagated_instance_ids,

        /* Provide a list of instance related to the object */
        COALESCE(json_agg(distinct instance_id) FILTER (WHERE instance_id IS NOT  NULL), '[]') as related_instance_ids
        FROM (
            SELECT
                i.id as instance_id,
                /*
                * Get the number of interfaces associated with the instance
                * that do not have NSGs on the interface.
                * When we allow per-interface NSGs, the count here would
                * just need to filter on ifc->network_security_group_id IS NULL,
                * and that would also need to be applied to the VPC propagation query.
                */
                count(distinct(ifc->'internal_uuid'))::int as interfaces_expected,
                /*
                * Get the count of interfaces where the source is INSTANCE
                * and the NSG ID and version matches those of the INSTANCE.
                */
                coalesce(sum(
                        (ifco #> '{network_security_group}'->>'source' = 'INSTANCE'
                        AND
                        ifco #> '{network_security_group}'->>'id' = i.network_security_group_id
                        AND
                        ifco #> '{network_security_group}'->>'version' = nsg.version)::int
                ), 0)::int as interfaces_applied
            FROM
                instances i
            JOIN jsonb_array_elements(i.network_config #>'{interfaces}') ifc on ifc->>'network_security_group_id' IS NULL
            JOIN machine_interfaces mi ON mi.machine_id = i.machine_id
            JOIN machines dpu ON dpu.id = mi.attached_dpu_machine_id
            /* network_status_observation is stored in dpu now. */
            LEFT OUTER JOIN jsonb_array_elements(dpu.network_status_observation #>'{instance_network_observation,interfaces}') ifco on ifco->>'internal_uuid' = ifc->>'internal_uuid'
            JOIN network_security_groups nsg on nsg.id=i.network_security_group_id
            WHERE i.deleted IS NULL");

    if network_security_group_ids.is_some() {
        instance_query_builder.push(" AND nsg.id = ANY(");
        instance_query_builder.push_bind(network_security_group_ids);
        instance_query_builder.push(") ");
    }

    if instance_ids.is_some() {
        instance_query_builder.push(" AND i.id = ANY(");
        instance_query_builder.push_bind(instance_ids);
        instance_query_builder.push(") ");
    }

    if tenant_organization_id.is_some() {
        instance_query_builder.push(" AND nsg.tenant_organization_id = ");
        instance_query_builder.push_bind(tenant_organization_id.map(|t| t.to_string()));
    }

    instance_query_builder.push(
        " GROUP BY i.id) as prop_stats GROUP BY instance_id,interfaces_expected,interfaces_applied",
    );

    let vpcs = vpc_query_builder
        .build_query_as()
        .fetch_all(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(vpc_query_builder.sql(), err))?;

    let instances = instance_query_builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(instance_query_builder.sql(), err))?;

    Ok((vpcs, instances))
}

/// Updates a NetworkSecurityGroup records in the DB.
///
/// * `txn`                    - A reference to an active DB transaction
/// * `id`                     - A reference to a NetworkSecurityGroupId to be set as the id
///   for the new NetworkSecurityGroup
/// * `tenant_organization_id` - A reference to a TenantOrganizationId for the tenant org that owns
///   this NetworkSecurityGroup.  The update will will be ignored if
///   this ID does not match that of the requested record.
///   ***Callers are expected to verify the relationship prior to
///   calling this function.***
/// * `metadata`               - A reference to a Metadata struct containing extra details about
///   the NetworkSecurityGroup
/// * `stateful_egress`        - Whether egress rules are stateful.
/// * `rules`                  - A slice of NetworkSecurityGroupRule containing the ACLs of the
///   NetworkSecurityGroup
/// * `expected_version`       - The version the record is expected to have prior to the update.
///   This will be auto-incremented. If this version passed in does not
///   match the reality of the record, the update will be rejected, but
///   ***callers are expected to have verified this version matches the record
///   in advance.***
/// * `updated_by`             - Optional String containing an ID to track the user who updated the
///   NetworkSecurityGroup
#[allow(clippy::too_many_arguments)]
pub async fn update(
    txn: &mut PgConnection,
    id: &NetworkSecurityGroupId,
    tenant_organization_id: &TenantOrganizationId,
    metadata: &Metadata,
    stateful_egress: bool,
    rules: &[NetworkSecurityGroupRule],
    expected_version: ConfigVersion,
    updated_by: Option<&str>,
) -> Result<NetworkSecurityGroup, DatabaseError> {
    let query = "UPDATE network_security_groups
            SET
                name=$1::varchar,
                labels=$2::jsonb,
                description=$3::varchar,
                rules=$4::jsonb,
                version=$5::varchar,
                updated_by=$6::varchar,
                stateful_egress=$10
            WHERE
                /*
                    All but the final `AND NOT EXISTS` are here to be defensive.
                    The cases should have already been covered by a query in advance.
                */
                id=$7::varchar
                AND version = $8::varchar
                AND deleted IS NULL
                AND tenant_organization_id = $9::varchar
                AND NOT EXISTS
                    (SELECT id FROM network_security_groups WHERE id!=$7::varchar AND name=$1::varchar AND deleted IS NULL)
            RETURNING *";

    match sqlx::query_as::<Postgres, NetworkSecurityGroup>(query)
        .bind(&metadata.name)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&rules))
        .bind(expected_version.increment())
        .bind(updated_by)
        .bind(id)
        .bind(expected_version)
        .bind(tenant_organization_id.to_string())
        .bind(stateful_egress)
        .fetch_one(txn)
        .await
    {
        Ok(network_security_group) => Ok(network_security_group),
        // This error should only show up when we didn't
        // update a record because the subquery found an existing name already.
        // deleted and version should have already been checked and reported
        // before calling update()
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::AlreadyFoundError {
            kind: "NetworkSecurityGroup",
            id: metadata.name.clone(),
        }),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

/// Soft deletes an instance type by updating the deleted column in the DB.
/// If the record with that ID is already deleted, nothing changes and Ok(None)
/// is returned.
///
/// This does ***NOT*** check for any associations with other objects like VPC, Instance, etc.
///
/// * `txn`                       - A reference to an active DB transaction
/// * `network_security_group_id` - An NetworkSecurityGroupId for the NetworkSecurityGroup to be
///   soft-deleted.
/// * `tenant_organization_id`    - A reference to a TenantOrganizationId of the tenant organization
///   that owns the NetworkSecurityGroup.  The delete
///   will be ignored if this ID does not match that of the requested record.
///   ***Callers are expected to verify the relationship prior to calling this function.***
pub async fn soft_delete(
    txn: &mut PgConnection,
    network_security_group_id: &NetworkSecurityGroupId,
    tenant_organization_id: &TenantOrganizationId,
) -> Result<Option<NetworkSecurityGroupId>, DatabaseError> {
    let query = "UPDATE network_security_groups SET deleted=NOW() WHERE id=$1::varchar AND tenant_organization_id=$2::varchar AND deleted is NULL RETURNING id";

    match sqlx::query_as(query)
        .bind(network_security_group_id)
        .bind(tenant_organization_id.to_string())
        .fetch_one(txn)
        .await
    {
        Ok(network_security_group) => Ok(Some(network_security_group)),
        Err(sqlx::Error::RowNotFound) => Ok(None),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}
