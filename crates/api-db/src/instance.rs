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
use std::str::FromStr;

use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{HostMachineId, MachineId};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use carbide_uuid::vpc::VpcId;
use chrono::prelude::*;
use config_version::ConfigVersion;
use model::instance::NewInstance;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::{InstanceNetworkConfig, InstanceNetworkConfigUpdate};
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::snapshot::{self, InstanceSnapshot, InstanceSnapshotPgJson};
use model::metadata::Metadata;
use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
use model::tenant::TenantOrganizationId;
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::db_read::DbReader;
use crate::operating_system::{self, OperatingSystem as OsRow};
use crate::{
    BIND_LIMIT, ColumnInfo, DatabaseError, DatabaseResult, FilterableQueryBuilder,
    ObjectColumnFilter, instance_address,
};

#[derive(Copy, Clone)]
pub struct IdColumn;

impl ColumnInfo<'_> for IdColumn {
    type TableType = InstanceTable;
    type ColumnType = InstanceId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InstanceTable {}

pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::instance::InstanceSearchFilter,
) -> Result<Vec<InstanceId>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT id FROM instances WHERE TRUE "); // The TRUE will be optimized away.
    push_search_filter(&mut builder, filter)?;

    let query = builder.build_query_as();
    query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("instance::find_ids", e))
}

/// Counts the instances matching `filter` without materializing their ids.
///
/// Callers that only need the number of matches use this instead of
/// `find_ids(..).len()`: it runs the same predicate but selects a scalar
/// `count(*)`, so the database returns a single row rather than one id per
/// matching instance. Shares its WHERE clause with [`find_ids`] via
/// [`push_search_filter`], so the two always agree on which rows match.
pub async fn count_ids(
    txn: impl DbReader<'_>,
    filter: model::instance::InstanceSearchFilter,
) -> Result<i64, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT count(*) FROM instances WHERE TRUE "); // The TRUE will be optimized away.
    push_search_filter(&mut builder, filter)?;

    builder
        .build_query_scalar()
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("instance::count_ids", e))
}

/// Adds the rows for every configuration that can still own network resources
/// for an instance. The current value is in `network_config`; both configurations
/// in `update_network_config_request` can still own resources until the update
/// releases the old ones.
///
/// A SLAAC interface can retain a prefix without creating a row in
/// `instance_addresses`, so searches and deletion checks cannot use address
/// rows as their only source. Callers must name the outer table `instances`
/// because this expression refers to it directly.
fn push_network_config_rows(builder: &mut sqlx::QueryBuilder<sqlx::Postgres>) {
    builder.push(
        "jsonb_array_elements(jsonb_build_array(
            instances.network_config,
            instances.update_network_config_request->'old_config',
            instances.update_network_config_request->'new_config'
        )) AS configs(config)",
    );
}

/// Adds a predicate that matches an instance whose configurations can still
/// own resources from `segment_id`.
pub(super) fn push_network_segment_reference_exists(
    builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
    segment_id: NetworkSegmentId,
) {
    builder.push(
        "EXISTS (
            SELECT 1
            FROM ",
    );
    push_network_config_rows(builder);
    builder.push(
        "
            WHERE EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    COALESCE(configs.config->'interfaces', '[]'::jsonb)
                ) AS interfaces(interface)
                WHERE (interfaces.interface->>'network_segment_id')::uuid = ",
    );
    builder.push_bind(segment_id);
    builder.push(
        "
            )
        )",
    );
}

/// Adds a predicate that matches an instance whose configurations can still
/// own resources from `vpc_id`. It deliberately excludes normalized rows in
/// `instance_addresses`; callers that need those rows add that predicate
/// separately.
fn push_network_config_vpc_reference_exists(
    builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
    vpc_id: VpcId,
) {
    builder.push(
        "EXISTS (
            SELECT 1
            FROM (SELECT ",
    );
    builder.push_bind(vpc_id);
    builder.push(
        "::uuid AS vpc_id) AS target
            WHERE EXISTS (
                SELECT 1
                FROM ",
    );
    push_network_config_rows(builder);
    builder.push(
        "
                WHERE EXISTS (
                    SELECT 1
                    FROM jsonb_array_elements(
                        COALESCE(configs.config->'interfaces', '[]'::jsonb)
                    ) AS interfaces(interface)
                    WHERE (interfaces.interface->>'vpc_id')::uuid = target.vpc_id
                    OR EXISTS (
                        SELECT 1
                        FROM network_segments
                        WHERE id = (interfaces.interface->>'network_segment_id')::uuid
                          AND vpc_id = target.vpc_id
                    )
                )
                OR (configs.config #>> '{auto_config,vpc_id}')::uuid = target.vpc_id
            )
        )",
    );
}

/// Adds the VPC predicate used by instance search.
///
/// An interface can name its VPC directly or through its network segment, and
/// an unresolved automatic configuration keeps the requested VPC in
/// `auto_config`. The address lookup preserves the existing behavior for
/// stateful allocations, while [`push_network_config_vpc_reference_exists`]
/// keeps SLAAC interfaces without host addresses visible.
fn push_vpc_search_filter(builder: &mut sqlx::QueryBuilder<sqlx::Postgres>, vpc_id: VpcId) {
    builder.push(
        " AND (
            EXISTS (
                SELECT 1
                FROM instance_addresses
                WHERE instance_addresses.instance_id = instances.id
                  AND instance_addresses.vpc_id = ",
    );
    builder.push_bind(vpc_id);
    builder.push(
        "
            )
            OR ",
    );
    push_network_config_vpc_reference_exists(builder, vpc_id);
    builder.push(")");
}

/// Appends the `InstanceSearchFilter` predicate onto a query builder whose SQL
/// already ends in `... WHERE TRUE `. Shared by [`find_ids`] and [`count_ids`]
/// so the row-returning and counting queries filter identically.
fn push_search_filter(
    builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
    filter: model::instance::InstanceSearchFilter,
) -> Result<(), DatabaseError> {
    if let Some(label) = filter.label {
        match (label.key.is_empty(), label.value) {
            // Label key is empty, label value is set.
            (true, Some(value)) => {
                builder.push(
                    " AND EXISTS (
                        SELECT 1
                        FROM jsonb_each_text(labels) AS kv
                        WHERE kv.value = ",
                );
                builder.push_bind(value);
                builder.push(")");
            }
            // Label key is empty, label value is not set.
            (true, None) => {
                return Err(DatabaseError::InvalidArgument(
                    "finding instances based on label needs either key or a value.".to_string(),
                ));
            }
            // Label key is not empty, label value is not set.
            (false, None) => {
                builder.push(" AND labels ->> ");
                builder.push_bind(label.key);
                builder.push(" IS NOT NULL");
            }
            // Label key is not empty, label value is set.
            (false, Some(value)) => {
                builder.push(" AND labels ->> ");
                builder.push_bind(label.key);
                builder.push(" = ");
                builder.push_bind(value);
            }
        }
    }

    if let Some(tenant_org_id) = filter.tenant_org_id {
        builder.push(" AND tenant_org = ");
        builder.push_bind(tenant_org_id);
    }

    if let Some(instance_type_id) = filter.instance_type_id {
        builder.push(" AND instance_type_id = ");
        builder.push_bind(instance_type_id);
    }

    if let Some(vpc_id) = filter.vpc_id {
        let vpc_id = VpcId::from_str(&vpc_id).map_err(DatabaseError::from)?;
        push_vpc_search_filter(builder, vpc_id);
    }

    Ok(())
}

pub async fn find(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'_, IdColumn>,
) -> Result<Vec<InstanceSnapshot>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new(
        "SELECT row_to_json(i.*) AS instance, row_to_json(o.*) AS operating_system \
         FROM instances i \
         LEFT JOIN operating_systems o ON i.operating_system_id = o.id AND o.deleted IS NULL",
    )
    .filter_relation(&filter, Some("i"));
    let rows: Vec<InstanceAndOsRow> = query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;
    rows.into_iter().map(InstanceSnapshot::try_from).collect()
}

/// Converts decoded snapshot rows to InstanceSnapshots, batch-loading OS
/// definitions as needed.
async fn resolve_snapshots_from_json_rows(
    txn: &mut PgConnection,
    rows: Vec<(Json<InstanceSnapshotPgJson>,)>,
) -> Result<Vec<InstanceSnapshot>, DatabaseError> {
    let pg_jsons: Vec<InstanceSnapshotPgJson> = rows.into_iter().map(|(json,)| json.0).collect();
    if pg_jsons.is_empty() {
        return Ok(Vec::new());
    }
    let os_ids: Vec<uuid::Uuid> = pg_jsons
        .iter()
        .filter_map(|p| p.operating_system_id)
        .collect();
    let os_by_id: std::collections::HashMap<uuid::Uuid, OsRow> = if os_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        operating_system::get_many(&mut *txn, &os_ids)
            .await?
            .into_iter()
            .map(|r| (r.id, r))
            .collect()
    };
    let mut snapshots = Vec::with_capacity(pg_jsons.len());
    for pg_json in pg_jsons {
        let snapshot = match pg_json.operating_system_id.and_then(|id| os_by_id.get(&id)) {
            Some(os_row) => {
                let os = build_operating_system_for_snapshot(os_row, &pg_json);
                snapshot::from_pg_json_and_os(pg_json, os).map_err(|e| DatabaseError::Internal {
                    message: format!("instance snapshot from_pg_json_and_os: {e}"),
                })?
            }
            None => InstanceSnapshot::try_from(pg_json).map_err(|e| DatabaseError::Internal {
                message: format!("instance snapshot try_from: {e}"),
            })?,
        };
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

/// Builds the effective OperatingSystem for an instance by merging the OS definition with instance overrides.
fn build_operating_system_for_snapshot(
    os_row: &OsRow,
    pg_json: &InstanceSnapshotPgJson,
) -> OperatingSystem {
    let user_data = pg_json
        .os_user_data
        .clone()
        .or_else(|| os_row.user_data.clone());
    let variant = match os_row.type_.as_str() {
        model::operating_system_definition::OS_TYPE_IPXE => {
            let script = if os_row.allow_override && !pg_json.os_ipxe_script.is_empty() {
                pg_json.os_ipxe_script.clone()
            } else {
                os_row.ipxe_script.clone().unwrap_or_default()
            };
            OperatingSystemVariant::Ipxe(InlineIpxe {
                ipxe_script: script,
            })
        }
        model::operating_system_definition::OS_TYPE_TEMPLATED_IPXE => {
            OperatingSystemVariant::OperatingSystemId(os_row.id)
        }
        _ => {
            tracing::warn!(
                operating_system_id = %os_row.id,
                os_type = %os_row.type_,
                "unexpected operating_system type, falling back to inline iPXE"
            );
            OperatingSystemVariant::Ipxe(InlineIpxe {
                ipxe_script: os_row
                    .ipxe_script
                    .clone()
                    .unwrap_or_else(|| pg_json.os_ipxe_script.clone()),
            })
        }
    };
    OperatingSystem {
        variant,
        user_data,
        phone_home_enabled: pg_json.os_phone_home_enabled,
        run_provisioning_instructions_on_every_boot: pg_json.os_always_boot_with_ipxe,
    }
}

/// Represents the data we get back from find_by_id and related functions that bring in the
/// operating system as well as the instance.
#[derive(sqlx::FromRow)]
struct InstanceAndOsRow {
    instance: Json<InstanceSnapshotPgJson>,
    operating_system: Option<Json<OsRow>>,
}

impl TryFrom<InstanceAndOsRow> for InstanceSnapshot {
    type Error = DatabaseError;
    fn try_from(pg_row: InstanceAndOsRow) -> Result<Self, Self::Error> {
        let instance_row = pg_row.instance.0;
        let os_row = pg_row.operating_system.map(|r| r.0);

        let snapshot = match os_row {
            Some(os_row) => {
                let os = build_operating_system_for_snapshot(&os_row, &instance_row);
                snapshot::from_pg_json_and_os(instance_row, os).map_err(|e| {
                    DatabaseError::Internal {
                        message: format!("instance snapshot from_pg_json_and_os: {e}"),
                    }
                })?
            }
            None => {
                // No OS reference: derive OS from instance columns only (legacy behavior).
                InstanceSnapshot::try_from(instance_row).map_err(|e| DatabaseError::Internal {
                    message: format!("instance snapshot try_from: {e}"),
                })?
            }
        };

        Ok(snapshot)
    }
}

pub async fn find_by_id(
    txn: impl DbReader<'_>,
    id: InstanceId,
) -> Result<Option<InstanceSnapshot>, DatabaseError> {
    // Single query; LEFT JOIN so we get instance even when operating_system_id is NULL.
    let query = "SELECT row_to_json(i.*) AS instance, row_to_json(o.*) AS operating_system
        FROM instances i
        LEFT JOIN operating_systems o ON i.operating_system_id = o.id AND o.deleted IS NULL
        WHERE i.id = $1";
    let Some(instance_and_os_row) = sqlx::query_as::<_, InstanceAndOsRow>(query)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
    else {
        return Ok(None);
    };
    Ok(Some(instance_and_os_row.try_into()?))
}

/// Instance data returned while its database record is locked for update.
#[derive(sqlx::FromRow)]
pub struct InstanceForUpdate {
    /// Instance identifier.
    pub id: InstanceId,
    /// Machine that owns the Instance.
    pub machine_id: HostMachineId,
    /// Tenant that owns the Instance.
    pub tenant_organization_id: TenantOrganizationId,
    /// Desired InfiniBand configuration stored on the Instance.
    #[sqlx(json)]
    pub infiniband_config: InstanceInfinibandConfig,
    /// Time at which deletion was requested, if any.
    pub deleted: Option<DateTime<Utc>>,
}

/// Finds an `Instance` by ID and locks its database record for update.
///
/// This includes an `Instance` already marked for deletion. The record remains
/// locked until `txn` ends.
pub async fn find_by_id_for_update(
    txn: &mut PgConnection,
    id: InstanceId,
) -> Result<Option<InstanceForUpdate>, DatabaseError> {
    let query = "SELECT i.id,
            i.machine_id,
            i.tenant_org AS tenant_organization_id,
            i.ib_config AS infiniband_config,
            i.deleted
        FROM instances i
        WHERE i.id = $1
        FOR UPDATE OF i";
    sqlx::query_as(query)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

pub async fn find_id_by_machine_id(
    db: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Option<InstanceId>, DatabaseError> {
    let query = "SELECT id from instances WHERE machine_id = $1";
    sqlx::query_as(query)
        .bind(machine_id)
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_by_machine_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<Option<InstanceSnapshot>, DatabaseError> {
    let Some(instance_id) = find_id_by_machine_id(&mut *txn, machine_id).await? else {
        return Ok(None);
    };
    find_by_id(txn, instance_id).await
}

/// Locks and returns the live `Instance` assigned to one `Machine`.
///
/// A returned record remains locked until the caller's transaction ends. `None`
/// means the `Machine` has no assigned `Instance`. If the assigned `Instance` is
/// already marked for deletion, the lookup returns
/// [`DatabaseError::FailedPrecondition`] instead of a snapshot that a caller
/// could use for later writes.
pub async fn find_live_by_machine_id_for_update(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<Option<InstanceSnapshot>, DatabaseError> {
    let query = "SELECT row_to_json(i.*) AS instance, row_to_json(o.*) AS operating_system
        FROM instances i
        LEFT JOIN operating_systems o ON i.operating_system_id = o.id AND o.deleted IS NULL
        WHERE i.machine_id = $1
        FOR UPDATE OF i";
    let Some(instance_and_os_row) = sqlx::query_as::<_, InstanceAndOsRow>(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
    else {
        return Ok(None);
    };
    let instance: InstanceSnapshot = instance_and_os_row.try_into()?;
    if instance.deleted.is_some() {
        return Err(DatabaseError::FailedPrecondition(format!(
            "instance {} is being deleted",
            instance.id
        )));
    }
    Ok(Some(instance))
}

pub async fn find_by_machine_ids(
    txn: &mut PgConnection,
    machine_ids: &[&HostMachineId],
) -> Result<Vec<InstanceSnapshot>, DatabaseError> {
    if machine_ids.is_empty() {
        return Ok(Vec::new());
    }
    let query = "SELECT row_to_json(i.*) from instances i WHERE machine_id = ANY($1)";
    let rows: Vec<(Json<InstanceSnapshotPgJson>,)> = sqlx::query_as(query)
        .bind(machine_ids)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    resolve_snapshots_from_json_rows(&mut *txn, rows).await
}

pub async fn find_by_extension_service(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    version: Option<ConfigVersion>,
) -> Result<Vec<InstanceSnapshot>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new(
        r#"SELECT row_to_json(i.*) FROM instances i
            WHERE i.deleted IS NULL AND EXISTS (
                SELECT 1
                FROM jsonb_array_elements(i.extension_services_config->'service_configs') AS es_config(cfg)
                WHERE cfg->>'service_id' =
        "#,
    );
    builder.push_bind(service_id.to_string());

    if let Some(version) = version {
        builder.push(" AND cfg->>'version' = ");
        builder.push_bind(version.to_string());
    }
    builder.push(")");

    let rows: Vec<(Json<InstanceSnapshotPgJson>,)> = builder
        .build_query_as()
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql().as_str(), e))?;
    resolve_snapshots_from_json_rows(txn, rows).await
}

/// Returns true if any non-deleted instance has this logical partition ID in
/// config.nvlink.gpu_configs[].logical_partition_id.
pub async fn any_instance_referencing_nvlink_logical_partition(
    txn: impl DbReader<'_>,
    logical_partition_id: &NvLinkLogicalPartitionId,
) -> Result<bool, DatabaseError> {
    let query = r#"SELECT EXISTS (
        SELECT 1 FROM instances
        WHERE deleted IS NULL
          AND nvlink_config->'gpu_configs' IS NOT NULL
          AND EXISTS (
            SELECT 1 FROM jsonb_array_elements(nvlink_config->'gpu_configs') AS gpu(g)
            WHERE g->>'logical_partition_id' = $1::text
          )
    )"#;
    sqlx::query_scalar(query)
        .bind(logical_partition_id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Counts instances whose current or pending network configuration references
/// `segment_id` through an interface.
///
/// Both configurations in a pending update can still own resources until the
/// update completes. Instances with a deletion timestamp remain owners until
/// physical deletion finishes termination. Each instance is counted at most
/// once even when several configurations or interfaces contain the same
/// reference.
pub async fn count_network_segment_references(
    txn: &mut PgConnection,
    segment_id: &NetworkSegmentId,
) -> Result<usize, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT count(*) FROM instances WHERE ");
    push_network_segment_reference_exists(&mut builder, *segment_id);

    let reference_count: i64 = builder
        .build_query_scalar()
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?;

    Ok(reference_count.max(0) as usize)
}

/// Counts instances whose current or pending network configuration refers to
/// `vpc_id` through an interface, its network segment, or an automatic
/// configuration request.
pub async fn count_vpc_references(
    txn: &mut PgConnection,
    vpc_id: &VpcId,
) -> Result<usize, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT count(*) FROM instances WHERE ");
    push_network_config_vpc_reference_exists(&mut builder, *vpc_id);

    let reference_count: i64 = builder
        .build_query_scalar()
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?;

    Ok(reference_count.max(0) as usize)
}

pub async fn use_custom_ipxe_on_next_boot(
    machine_id: &HostMachineId,
    boot_with_custom_ipxe: bool,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE instances SET use_custom_pxe_on_boot=$1::bool WHERE machine_id=$2 RETURNING machine_id";
    // Fetch one to make sure atleast one row is updated.
    let _: (MachineId,) = sqlx::query_as(query)
        .bind(boot_with_custom_ipxe)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Sets the custom_pxe_reboot_requested flag. This flag is set by the API when a tenant
/// requests a reboot with custom iPXE. The Ready handler checks this flag to initiate
/// the HostPlatformConfiguration flow. The WaitingForRebootToReady handler clears this
/// flag after setting use_custom_pxe_on_boot.
pub async fn set_custom_pxe_reboot_requested(
    machine_id: &HostMachineId,
    requested: bool,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE instances SET custom_pxe_reboot_requested=$1::bool WHERE machine_id=$2 RETURNING machine_id";
    let _: (MachineId,) = sqlx::query_as(query)
        .bind(requested)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Updates the desired network configuration for an instance
pub async fn update_network_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    new_state: &InstanceNetworkConfig,
    increment_version: bool,
) -> Result<(), DatabaseError> {
    batch_update_network_config(
        txn,
        &[(instance_id, expected_version, new_state)],
        increment_version,
    )
    .await
}

/// Distinguishes a missing or deleted `Instance` from a version conflict after
/// an optimistic configuration update affects no records.
async fn ensure_live_for_config_update(
    txn: &mut PgConnection,
    instance_id: InstanceId,
) -> Result<(), DatabaseError> {
    let query = "SELECT deleted IS NULL FROM instances WHERE id=$1 FOR UPDATE";
    let live: Option<bool> = sqlx::query_scalar(query)
        .bind(instance_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    match live {
        Some(true) => Ok(()),
        Some(false) => Err(DatabaseError::FailedPrecondition(format!(
            "instance {instance_id} is being deleted"
        ))),
        None => Err(DatabaseError::FailedPrecondition(format!(
            "instance {instance_id} does not exist"
        ))),
    }
}

pub async fn update_phone_home_last_contact(
    txn: &mut PgConnection,
    instance_id: InstanceId,
) -> Result<DateTime<Utc>, DatabaseError> {
    let query = "UPDATE instances SET phone_home_last_contact=now() WHERE id=$1 RETURNING phone_home_last_contact";

    let query_result: (DateTime<Utc>,) = sqlx::query_as::<_, (DateTime<Utc>,)>(query) // Specify return type
        .bind(instance_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    tracing::info!(
        instance_id = %instance_id,
        phone_home_last_contact = %query_result.0,
        "Phone home last contact updated",
    );
    Ok(query_result.0)
}

pub async fn clear_phone_home_last_contact(
    txn: &mut PgConnection,
    instance_id: InstanceId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE instances SET phone_home_last_contact=NULL WHERE id=$1 RETURNING id";

    let _id = sqlx::query_as::<_, InstanceId>(query)
        .bind(instance_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    tracing::info!(
        instance_id = %instance_id,
        "Phone home last contact cleared",
    );
    Ok(())
}

/// Updates updateable configurations of an instance
/// - OS
/// - Keyset IDs
/// - Metadata
/// - Security Group
///
/// This method will not update
/// - instance network and infiniband configurations
/// - tenant organization IDs
///
/// The update applies only while the instance exists, is not marked deleted,
/// and still has `expected_version`. A deleted or missing instance reports a
/// failed precondition; a live instance with another version reports a
/// concurrent modification.
pub async fn update_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    config: InstanceConfig,
    metadata: Metadata,
) -> Result<(), DatabaseError> {
    let next_version = expected_version.increment();

    let mut os_ipxe_script = String::new();
    let os_user_data = config.os.user_data;
    let mut os_image_id = None;
    let operating_system_id = match &config.os.variant {
        OperatingSystemVariant::Ipxe(ipxe) => {
            os_ipxe_script = ipxe.ipxe_script.clone();
            None
        }
        OperatingSystemVariant::OsImage(id) => {
            os_image_id = Some(id);
            None
        }
        OperatingSystemVariant::OperatingSystemId(id) => Some(*id),
    };

    let query = "UPDATE instances SET config_version=$1,
            operating_system_id=$2, os_ipxe_script=$3, os_user_data=$4, os_always_boot_with_ipxe=$5, os_phone_home_enabled=$6,
            os_image_id=$7, keyset_ids=$8,
            name=$9, description=$10, labels=$11::json, network_security_group_id=$14,
            power_profile=$15
            WHERE id=$12 AND config_version=$13 AND deleted IS NULL
            RETURNING id";
    let query_result: Result<(InstanceId,), _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(operating_system_id)
        .bind(os_ipxe_script)
        .bind(os_user_data)
        .bind(config.os.run_provisioning_instructions_on_every_boot)
        .bind(config.os.phone_home_enabled)
        .bind(os_image_id)
        .bind(config.tenant.tenant_keyset_ids)
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(instance_id)
        .bind(expected_version)
        .bind(config.network_security_group_id)
        .bind(config.power_profile)
        .fetch_one(&mut *txn)
        .await;

    match query_result {
        Ok((_instance_id,)) => Ok(()),
        Err(sqlx::Error::RowNotFound) => {
            ensure_live_for_config_update(txn, instance_id).await?;
            Err(DatabaseError::ConcurrentModificationError(
                "instance",
                expected_version.to_string(),
            ))
        }
        Err(error) => Err(DatabaseError::query(query, error)),
    }
}

/// Updates the Operating System
///
/// The update applies only while the instance exists, is not marked deleted,
/// and still has `expected_version`. A deleted or missing instance reports a
/// failed precondition; a live instance with another version reports a
/// concurrent modification.
pub async fn update_os(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    os: OperatingSystem,
) -> Result<(), DatabaseError> {
    let next_version = expected_version.increment();

    let mut os_ipxe_script = String::new();
    let os_user_data = os.user_data;
    let mut os_image_id = None;
    let operating_system_id = match &os.variant {
        OperatingSystemVariant::Ipxe(ipxe) => {
            os_ipxe_script = ipxe.ipxe_script.clone();
            None
        }
        OperatingSystemVariant::OsImage(id) => {
            os_image_id = Some(id);
            None
        }
        OperatingSystemVariant::OperatingSystemId(id) => Some(*id),
    };

    let query = "UPDATE instances SET config_version=$1,
            operating_system_id=$2, os_ipxe_script=$3, os_user_data=$4, os_always_boot_with_ipxe=$5, os_phone_home_enabled=$6, os_image_id=$7
            WHERE id=$8 AND config_version=$9 AND deleted IS NULL
            RETURNING id";
    let query_result: Result<(InstanceId,), _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(operating_system_id)
        .bind(os_ipxe_script)
        .bind(os_user_data)
        .bind(os.run_provisioning_instructions_on_every_boot)
        .bind(os.phone_home_enabled)
        .bind(os_image_id)
        .bind(instance_id)
        .bind(expected_version)
        .fetch_one(&mut *txn)
        .await;

    match query_result {
        Ok((_instance_id,)) => Ok(()),
        Err(sqlx::Error::RowNotFound) => {
            ensure_live_for_config_update(txn, instance_id).await?;
            Err(DatabaseError::ConcurrentModificationError(
                "instance",
                expected_version.to_string(),
            ))
        }
        Err(error) => Err(DatabaseError::query(query, error)),
    }
}

/// Updates the desired infiniband configuration for an instance
pub async fn update_ib_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    new_state: &InstanceInfinibandConfig,
    increment_version: bool,
) -> Result<(), DatabaseError> {
    batch_update_ib_config(
        txn,
        &[(instance_id, expected_version, new_state)],
        increment_version,
    )
    .await
}

/// Updates the desired nvlink configuration for an instance
pub async fn update_nvlink_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    new_state: &InstanceNvLinkConfig,
    increment_version: bool,
) -> Result<(), DatabaseError> {
    batch_update_nvlink_config(
        txn,
        &[(instance_id, expected_version, new_state)],
        increment_version,
    )
    .await
}

/// Updates the desired spx configuration for an instance
pub async fn update_spx_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    new_state: &InstanceSpxConfig,
    increment_version: bool,
) -> Result<(), DatabaseError> {
    batch_update_spx_config(
        txn,
        &[(instance_id, expected_version, new_state)],
        increment_version,
    )
    .await
}

pub async fn trigger_update_network_config_request(
    instance_id: &InstanceId,
    current: &InstanceNetworkConfig,
    requested: &InstanceNetworkConfig,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), DatabaseError> {
    let network_config_request = InstanceNetworkConfigUpdate {
        old_config: current.clone(),
        new_config: requested.clone(),
    };
    let query = r#"UPDATE instances SET update_network_config_request=$1::json
                        WHERE id = $2::uuid
                          AND update_network_config_request IS NULL
                          AND deleted IS NULL
                        RETURNING id"#;
    let (_,): (InstanceId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(network_config_request))
        .bind(instance_id)
        .fetch_one(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn delete_update_network_config_request(
    instance_id: &InstanceId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE instances SET update_network_config_request=NULL 
                        WHERE id = $1::uuid"#;
    sqlx::query(query)
        .bind(instance_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_extension_services_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    new_config: &InstanceExtensionServicesConfig,
    increment_version: bool,
) -> Result<(), DatabaseError> {
    let next_version = if increment_version {
        expected_version.increment()
    } else {
        expected_version
    };

    let query = "UPDATE instances SET extension_services_config_version=$1, extension_services_config=$2::json
        WHERE id=$3 AND extension_services_config_version=$4
        RETURNING id";
    let query_result: Result<(InstanceId,), _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(sqlx::types::Json(new_config))
        .bind(instance_id)
        .bind(expected_version)
        .fetch_one(txn)
        .await;

    match query_result {
        Ok((_instance_id,)) => Ok(()),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

/// Each `batch_persist` VALUES row binds this many parameters. Postgres caps
/// a single statement at 65535 bind parameters, so an unchunked INSERT
/// overflows once `values.len() * BATCH_PERSIST_BINDS_PER_ROW` crosses that
/// cap (~2.3k rows) -- a single `--transactional` allocate of 4,500 hosts
/// (126k binds) would fail outright. `batch_persist` chunks the INSERT into
/// sub-batches of `BIND_LIMIT / BATCH_PERSIST_BINDS_PER_ROW` rows, all issued
/// on the caller's transaction so the write stays all-or-nothing.
const BATCH_PERSIST_BINDS_PER_ROW: usize = 28;

/// Batch insert for multiple instances.
/// This is optimized for inserting many instances in a single database operation.
///
/// Note: This function expects all machines to exist and be locked before calling.
/// It will fail if any machine doesn't exist or has a mismatched instance_type_id.
pub async fn batch_persist<'a>(
    values: Vec<NewInstance<'a>>,
    txn: &mut PgConnection,
) -> DatabaseResult<Vec<InstanceSnapshot>> {
    if values.is_empty() {
        return Ok(Vec::new());
    }

    // For batch insert, we need to collect all the instance IDs and then query them back
    // because Postgres INSERT ... RETURNING with push_values doesn't work well with row_to_json
    let instance_ids: Vec<InstanceId> = values.iter().map(|v| v.instance_id).collect();

    let query = "INSERT INTO instances (
                        id,
                        machine_id,
                        operating_system_id,
                        os_user_data,
                        os_ipxe_script,
                        os_image_id,
                        os_always_boot_with_ipxe,
                        tenant_org,
                        network_config,
                        network_config_version,
                        ib_config,
                        ib_config_version,
                        keyset_ids,
                        os_phone_home_enabled,
                        name,
                        description,
                        labels,
                        config_version,
                        hostname,
                        network_security_group_id,
                        use_custom_pxe_on_boot,
                        instance_type_id,
                        extension_services_config,
                        extension_services_config_version,
                        nvlink_config,
                        nvlink_config_version,
                        spx_config,
                        spx_config_version,
                        power_profile
                    )
                    SELECT 
                            vals.id, vals.machine_id, vals.operating_system_id, vals.os_user_data, vals.os_ipxe_script,
                            vals.os_image_id, vals.os_always_boot_with_ipxe, vals.tenant_org, 
                            vals.network_config::json, vals.network_config_version, 
                            vals.ib_config::json, vals.ib_config_version, vals.keyset_ids, 
                            vals.os_phone_home_enabled, vals.name, vals.description, 
                            vals.labels::json, vals.config_version, vals.hostname, 
                            vals.network_security_group_id, true,
                            vals.instance_type_id, vals.extension_services_config::json, 
                            vals.extension_services_config_version, vals.nvlink_config::json, 
                            vals.nvlink_config_version, vals.spx_config::json, vals.spx_config_version,
                            vals.power_profile
                    FROM (VALUES ";

    let expected_count = values.len() as u64;
    let mut rows_affected_total: u64 = 0;

    for chunk in values.chunks(BIND_LIMIT / BATCH_PERSIST_BINDS_PER_ROW) {
        let mut qb = sqlx::QueryBuilder::new(query);

        // Build VALUES clause
        let mut separated = qb.separated(", ");
        for value in chunk {
            let mut os_ipxe_script = String::new();
            let os_user_data = value.config.os.user_data.clone();
            let mut os_image_id: Option<uuid::Uuid> = None;
            let operating_system_id = match &value.config.os.variant {
                OperatingSystemVariant::Ipxe(ipxe) => {
                    os_ipxe_script = ipxe.ipxe_script.clone();
                    None
                }
                OperatingSystemVariant::OsImage(id) => {
                    os_image_id = Some(*id);
                    None
                }
                OperatingSystemVariant::OperatingSystemId(id) => Some(*id),
            };

            separated.push("(");
            separated.push_bind_unseparated(value.instance_id);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.machine_id.to_string());
            separated.push_unseparated(",");
            separated.push_bind_unseparated(operating_system_id);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(os_user_data);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(os_ipxe_script);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(os_image_id);
            separated.push_unseparated(",");
            separated
                .push_bind_unseparated(value.config.os.run_provisioning_instructions_on_every_boot);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.config.tenant.tenant_organization_id.as_str());
            separated.push_unseparated(",");
            separated.push_bind_unseparated(
                serde_json::to_string(&value.config.network).unwrap_or_default(),
            );
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.network_config_version);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(
                serde_json::to_string(&value.config.infiniband).unwrap_or_default(),
            );
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.ib_config_version);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(&value.config.tenant.tenant_keyset_ids);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.config.os.phone_home_enabled);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(&value.metadata.name);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(&value.metadata.description);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(
                serde_json::to_string(&value.metadata.labels).unwrap_or_default(),
            );
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.config_version);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(&value.config.tenant.hostname);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(&value.config.network_security_group_id);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(&value.instance_type_id);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(
                serde_json::to_string(&value.config.extension_services).unwrap_or_default(),
            );
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.extension_services_config_version);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(
                serde_json::to_string(&value.config.nvlink).unwrap_or_default(),
            );
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.nvlink_config_version);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(
                serde_json::to_string(&value.config.spxconfig).unwrap_or_default(),
            );
            separated.push_unseparated(",");
            separated.push_bind_unseparated(value.spx_config_version);
            separated.push_unseparated(",");
            separated.push_bind_unseparated(&value.config.power_profile);
            separated.push_unseparated(")");
        }

        qb.push(") AS vals(id, machine_id, operating_system_id, os_user_data, os_ipxe_script, os_image_id,
                       os_always_boot_with_ipxe, tenant_org, network_config, network_config_version,
                       ib_config, ib_config_version, keyset_ids, os_phone_home_enabled, name,
                       description, labels, config_version, hostname, network_security_group_id,
                       instance_type_id, extension_services_config, extension_services_config_version,
                       nvlink_config, nvlink_config_version, spx_config, spx_config_version,
                       power_profile)
            INNER JOIN machines m ON m.id = vals.machine_id
                AND (vals.instance_type_id IS NULL OR m.instance_type_id = vals.instance_type_id)");

        let result = qb
            .build()
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::new("batch_persist", e))?;

        rows_affected_total += result.rows_affected();
    }

    // Check if all instances were inserted
    // If instance_type_id doesn't match, the row won't be inserted due to the JOIN condition
    if rows_affected_total != expected_count {
        return Err(DatabaseError::FailedPrecondition(
            "expected InstanceTypeId does not match source machine".to_string(),
        ));
    }

    // Fetch the inserted instances, resolving OS definitions as needed.
    let query = "SELECT row_to_json(i.*) FROM instances i WHERE i.id = ANY($1)";
    let rows: Vec<(Json<InstanceSnapshotPgJson>,)> = sqlx::query_as(query)
        .bind(&instance_ids)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    resolve_snapshots_from_json_rows(txn, rows).await
}

/// Batch update network configs for multiple instances
/// Each update contains (instance_id, expected_version, config)
/// The increment_version flag controls whether to increment the version
pub async fn batch_update_network_config(
    txn: &mut PgConnection,
    updates: &[(InstanceId, ConfigVersion, &InstanceNetworkConfig)],
    increment_version: bool,
) -> Result<(), DatabaseError> {
    if updates.is_empty() {
        return Ok(());
    }

    let expected_count = updates.len() as u64;

    // Use a CTE to batch update with version check
    let mut qb = sqlx::QueryBuilder::new(
        "UPDATE instances SET 
            network_config_version = updates.new_version,
            network_config = updates.config::json
        FROM (VALUES ",
    );

    let mut separated = qb.separated(", ");
    for (instance_id, expected_version, config) in updates {
        // Compute new_version per-row (ConfigVersion is a complex struct, can't do arithmetic in SQL)
        let new_version = if increment_version {
            expected_version.increment()
        } else {
            *expected_version
        };
        separated.push("(");
        separated.push_bind_unseparated(*instance_id);
        separated.push_unseparated("::uuid,");
        separated.push_bind_unseparated(*expected_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(new_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(serde_json::to_string(config).unwrap_or_default());
        separated.push_unseparated(")");
    }

    qb.push(
        ") AS updates(id, expected_version, new_version, config) 
        WHERE instances.id = updates.id 
        AND instances.network_config_version = updates.expected_version",
    );

    let result = qb
        .build()
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("batch_update_network_config", e))?;

    // Verify all rows were updated (version check passed)
    if result.rows_affected() != expected_count {
        return Err(DatabaseError::FailedPrecondition(
            "Network config version mismatch during batch update".to_string(),
        ));
    }

    Ok(())
}

/// Batch update IB configs for multiple instances
/// Each update contains (instance_id, expected_version, config)
pub async fn batch_update_ib_config(
    txn: &mut PgConnection,
    updates: &[(InstanceId, ConfigVersion, &InstanceInfinibandConfig)],
    increment_version: bool,
) -> Result<(), DatabaseError> {
    if updates.is_empty() {
        return Ok(());
    }

    let expected_count = updates.len() as u64;

    let mut qb = sqlx::QueryBuilder::new(
        "UPDATE instances SET 
            ib_config_version = updates.new_version,
            ib_config = updates.config::json
        FROM (VALUES ",
    );

    let mut separated = qb.separated(", ");
    for (instance_id, expected_version, config) in updates {
        let new_version = if increment_version {
            expected_version.increment()
        } else {
            *expected_version
        };
        separated.push("(");
        separated.push_bind_unseparated(*instance_id);
        separated.push_unseparated("::uuid,");
        separated.push_bind_unseparated(*expected_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(new_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(serde_json::to_string(config).unwrap_or_default());
        separated.push_unseparated(")");
    }

    qb.push(
        ") AS updates(id, expected_version, new_version, config) 
        WHERE instances.id = updates.id 
        AND instances.ib_config_version = updates.expected_version",
    );

    let result = qb
        .build()
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("batch_update_ib_config", e))?;

    // Verify all rows were updated (version check passed)
    if result.rows_affected() != expected_count {
        return Err(DatabaseError::FailedPrecondition(
            "IB config version mismatch during batch update".to_string(),
        ));
    }

    Ok(())
}

/// Batch update nvlink configs for multiple instances
/// Each update contains (instance_id, expected_version, config)
pub async fn batch_update_nvlink_config(
    txn: &mut PgConnection,
    updates: &[(InstanceId, ConfigVersion, &InstanceNvLinkConfig)],
    increment_version: bool,
) -> Result<(), DatabaseError> {
    if updates.is_empty() {
        return Ok(());
    }

    let expected_count = updates.len() as u64;

    let mut qb = sqlx::QueryBuilder::new(
        "UPDATE instances SET 
            nvlink_config_version = updates.new_version,
            nvlink_config = updates.config::json
        FROM (VALUES ",
    );

    let mut separated = qb.separated(", ");
    for (instance_id, expected_version, config) in updates {
        let new_version = if increment_version {
            expected_version.increment()
        } else {
            *expected_version
        };
        separated.push("(");
        separated.push_bind_unseparated(*instance_id);
        separated.push_unseparated("::uuid,");
        separated.push_bind_unseparated(*expected_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(new_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(serde_json::to_string(config).unwrap_or_default());
        separated.push_unseparated(")");
    }

    qb.push(
        ") AS updates(id, expected_version, new_version, config) 
        WHERE instances.id = updates.id 
        AND instances.nvlink_config_version = updates.expected_version",
    );

    let result = qb
        .build()
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("batch_update_nvlink_config", e))?;

    // Verify all rows were updated (version check passed)
    if result.rows_affected() != expected_count {
        return Err(DatabaseError::FailedPrecondition(
            "NVLink config version mismatch during batch update".to_string(),
        ));
    }

    Ok(())
}

/// Batch update spx configs for multiple instances
/// Each update contains (instance_id, expected_version, config)
pub async fn batch_update_spx_config(
    txn: &mut PgConnection,
    updates: &[(InstanceId, ConfigVersion, &InstanceSpxConfig)],
    increment_version: bool,
) -> Result<(), DatabaseError> {
    if updates.is_empty() {
        return Ok(());
    }

    let expected_count = updates.len() as u64;

    let mut qb = sqlx::QueryBuilder::new(
        "UPDATE instances SET 
            spx_config_version = updates.new_version,
            spx_config = updates.config::json
        FROM (VALUES ",
    );

    let mut separated = qb.separated(", ");
    for (instance_id, expected_version, config) in updates {
        let new_version = if increment_version {
            expected_version.increment()
        } else {
            *expected_version
        };
        separated.push("(");
        separated.push_bind_unseparated(*instance_id);
        separated.push_unseparated("::uuid,");
        separated.push_bind_unseparated(*expected_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(new_version);
        separated.push_unseparated(",");
        separated.push_bind_unseparated(serde_json::to_string(config).unwrap_or_default());
        separated.push_unseparated(")");
    }

    qb.push(
        ") AS updates(id, expected_version, new_version, config) 
        WHERE instances.id = updates.id 
        AND instances.spx_config_version = updates.expected_version",
    );

    let result = qb
        .build()
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("batch_update_spx_config", e))?;

    // Verify all rows were updated (version check passed)
    if result.rows_affected() != expected_count {
        tracing::error!(
            affected_row_count = result.rows_affected(),
            expected_row_count = expected_count,
            "SPX config batch update affected an unexpected number of rows",
        );
        return Err(DatabaseError::FailedPrecondition(
            "Spx config version mismatch during batch update".to_string(),
        ));
    }

    Ok(())
}

pub async fn delete(instance_id: InstanceId, txn: &mut PgConnection) -> DatabaseResult<()> {
    instance_address::delete(&mut *txn, instance_id).await?;

    let query = "DELETE FROM instances where id=$1::uuid RETURNING id";
    sqlx::query_as::<_, InstanceId>(query)
        .bind(instance_id)
        .fetch_one(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn mark_as_deleted(
    instance_id: InstanceId,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let query = "UPDATE instances SET deleted=NOW() WHERE id=$1::uuid RETURNING id";

    let _id = sqlx::query_as::<_, InstanceId>(query)
        .bind(instance_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    const INSTANCE_IPXE_SCRIPT: &str = "#!ipxe boot-from-instance-columns";

    /// Seeds a machine plus an instance on it with the given OS reference and
    /// an inline iPXE script in the instance columns, returning the instance id.
    async fn seed_instance(
        conn: &mut PgConnection,
        machine_seed: u8,
        operating_system_id: Option<uuid::Uuid>,
    ) -> InstanceId {
        let machine_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [machine_seed; 32],
            MachineType::Host,
        );
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query_scalar(
            "INSERT INTO instances \
             (machine_id, operating_system_id, os_ipxe_script, network_config, nvlink_config) \
             VALUES ($1, $2, $3, '{\"interfaces\": []}'::jsonb, '{\"gpu_configs\": []}'::jsonb) \
             RETURNING id",
        )
        .bind(machine_id)
        .bind(operating_system_id)
        .bind(INSTANCE_IPXE_SCRIPT)
        .fetch_one(conn)
        .await
        .unwrap()
    }

    /// Seeds `n` bare machines (no FK dependents besides `dpf`), each with a
    /// distinct id derived from its index, in a single multi-row INSERT.
    async fn seed_machines(conn: &mut PgConnection, n: usize) -> Vec<HostMachineId> {
        let machine_ids: Vec<HostMachineId> = (0..n)
            .map(|i| {
                let mut hardware_hash = [0u8; 32];
                hardware_hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
                MachineId::new(
                    MachineIdSource::ProductBoardChassisSerial,
                    hardware_hash,
                    MachineType::Host,
                )
                .try_into()
                .unwrap()
            })
            .collect();

        let mut qb = sqlx::QueryBuilder::new("INSERT INTO machines (id, dpf) ");
        qb.push_values(machine_ids.iter(), |mut b, machine_id| {
            b.push_bind(*machine_id).push("'{}'::jsonb");
        });
        qb.build().execute(&mut *conn).await.unwrap();

        machine_ids
    }

    /// Builds a minimal-but-valid `NewInstance` on `machine_id`, distinct from
    /// every other instance produced by this helper via `instance_id`.
    fn new_instance(machine_id: HostMachineId, config: &InstanceConfig) -> NewInstance<'_> {
        let version = ConfigVersion::initial();
        NewInstance {
            instance_id: InstanceId::new(),
            machine_id,
            instance_type_id: None,
            config,
            metadata: Metadata::default(),
            config_version: version,
            network_config_version: version,
            ib_config_version: version,
            extension_services_config_version: version,
            nvlink_config_version: version,
            spx_config_version: version,
        }
    }

    fn minimal_instance_config() -> InstanceConfig {
        InstanceConfig {
            tenant: model::instance::config::tenant_config::TenantConfig {
                tenant_organization_id: TenantOrganizationId::try_from(
                    "batch-persist-chunking".to_string(),
                )
                .unwrap(),
                tenant_keyset_ids: Vec::new(),
                hostname: None,
            },
            os: OperatingSystem {
                user_data: None,
                variant: OperatingSystemVariant::Ipxe(InlineIpxe {
                    ipxe_script: "#!ipxe".to_string(),
                }),
                phone_home_enabled: false,
                run_provisioning_instructions_on_every_boot: false,
            },
            network: InstanceNetworkConfig::default(),
            infiniband: InstanceInfinibandConfig::default(),
            network_security_group_id: None,
            extension_services: InstanceExtensionServicesConfig::default(),
            nvlink: InstanceNvLinkConfig::default(),
            spxconfig: InstanceSpxConfig::default(),
            power_profile: None,
        }
    }

    /// `batch_persist` chunks its INSERT at `BIND_LIMIT / BATCH_PERSIST_BINDS_PER_ROW`
    /// rows to stay under Postgres's bind-parameter ceiling (see the constant's
    /// doc comment). This is the regression guard for that chunking: a batch
    /// one row larger than a single chunk must still persist every row --
    /// each sub-batch INSERT commits on the same caller-supplied transaction,
    /// so the whole call remains all-or-nothing even though it issues more
    /// than one statement. Before chunking existed, this row count blew the
    /// 65535 bind-parameter limit and `batch_persist` failed outright.
    #[crate::sqlx_test]
    async fn batch_persist_splits_across_bind_limit_chunks(pool: sqlx::PgPool) {
        let chunk_size = BIND_LIMIT / BATCH_PERSIST_BINDS_PER_ROW;
        let row_count = chunk_size + 1; // one row past the first chunk boundary

        let mut txn = pool.begin().await.unwrap();
        let machine_ids = seed_machines(&mut txn, row_count).await;
        let config = minimal_instance_config();
        let values: Vec<NewInstance> = machine_ids
            .iter()
            .map(|&machine_id| new_instance(machine_id, &config))
            .collect();

        let snapshots = batch_persist(values, &mut txn).await.unwrap();
        assert_eq!(
            snapshots.len(),
            row_count,
            "every row across both chunks should come back in the result"
        );

        let persisted: i64 = sqlx::query_scalar("SELECT count(*) FROM instances")
            .fetch_one(&mut *txn)
            .await
            .unwrap();
        assert_eq!(
            persisted, row_count as i64,
            "every row across both chunks should be committed, not just the first chunk"
        );
    }

    /// `batch_persist(vec![])` must be a no-op rather than building a
    /// zero-row `VALUES (...)` clause (which is invalid SQL).
    #[crate::sqlx_test]
    async fn batch_persist_handles_empty_batch(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let snapshots = batch_persist(Vec::new(), &mut txn).await.unwrap();
        assert!(snapshots.is_empty());
    }

    /// Pins the SQL NULL semantics the `Option<Json<OsRow>>` decode relies on:
    /// when the LEFT JOIN finds no live operating_systems row, Postgres
    /// projects `row_to_json(o.*)` as SQL NULL (decoded as `None`), not as a
    /// JSON object whose fields are all null (which would fail to decode into
    /// `OsRow`'s non-optional fields). Covers both unmatched-join paths: an
    /// instance with no OS reference at all, and an instance whose referenced
    /// OS is soft-deleted and filtered out by the join's `o.deleted IS NULL`.
    #[crate::sqlx_test]
    async fn unmatched_os_join_decodes_as_none(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        let no_os_ref = seed_instance(&mut txn, 0x42, None).await;

        let os = operating_system::create(
            &mut txn,
            &operating_system::CreateOperatingSystem {
                id: None,
                name: "soft-deleted-os".to_string(),
                description: None,
                org: Some("test-org".to_string()),
                type_: model::operating_system_definition::OS_TYPE_IPXE.to_string(),
                status: operating_system::OS_STATUS_READY.to_string(),
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("#!ipxe boot-from-os-row".to_string()),
                ipxe_template_id: None,
                ipxe_parameters: None,
                ipxe_artifacts: None,
                ipxe_definition_hash: None,
            },
        )
        .await
        .unwrap();
        operating_system::delete(&mut txn, os.id).await.unwrap();
        let deleted_os_ref = seed_instance(&mut txn, 0x43, Some(os.id)).await;

        // find_by_id loads both instances without a live OS row: the
        // reference-free instance derives its OS from the inline script
        // column, and the soft-deleted reference keeps the id-only variant
        // rather than decoding (or erroring on) the deleted row's contents.
        let snapshot = find_by_id(&mut *txn, no_os_ref).await.unwrap().unwrap();
        assert_eq!(
            snapshot.config.os.variant,
            OperatingSystemVariant::Ipxe(InlineIpxe {
                ipxe_script: INSTANCE_IPXE_SCRIPT.to_string()
            })
        );
        let snapshot = find_by_id(&mut *txn, deleted_os_ref)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.config.os.variant,
            OperatingSystemVariant::OperatingSystemId(os.id)
        );

        // find() projects the OS column the same way; both instances decode.
        let ids = [no_os_ref, deleted_os_ref];
        let snapshots = find(&mut *txn, ObjectColumnFilter::List(IdColumn, &ids))
            .await
            .unwrap();
        assert_eq!(snapshots.len(), 2);
    }

    /// General and OS updates distinguish missing and deleted `Instance`s from
    /// live records whose version has changed.
    #[crate::sqlx_test]
    async fn config_writers_return_distinct_errors_for_missing_deleted_and_stale_records(
        pool: sqlx::PgPool,
    ) {
        enum StaleUpdate {
            Config,
            OperatingSystem,
        }

        enum RowState {
            Deleted,
            Missing,
            LiveWithNewerVersion,
        }

        let cases = [
            (
                "deleted general config",
                0x44,
                StaleUpdate::Config,
                RowState::Deleted,
            ),
            (
                "deleted operating system",
                0x45,
                StaleUpdate::OperatingSystem,
                RowState::Deleted,
            ),
            (
                "live stale general config",
                0x46,
                StaleUpdate::Config,
                RowState::LiveWithNewerVersion,
            ),
            (
                "live stale operating system",
                0x47,
                StaleUpdate::OperatingSystem,
                RowState::LiveWithNewerVersion,
            ),
            (
                "missing general config",
                0x48,
                StaleUpdate::Config,
                RowState::Missing,
            ),
            (
                "missing operating system",
                0x49,
                StaleUpdate::OperatingSystem,
                RowState::Missing,
            ),
        ];

        for (case_name, machine_seed, stale_update, row_state) in cases {
            let mut setup = pool.begin().await.unwrap();
            let instance_id = seed_instance(&mut setup, machine_seed, None).await;
            setup.commit().await.unwrap();

            let stale_snapshot = find_by_id(&pool, instance_id).await.unwrap().unwrap();
            let expected_version = stale_snapshot.config_version;
            let mut prepare = pool.begin().await.unwrap();
            let persisted_version = match row_state {
                RowState::Deleted => {
                    mark_as_deleted(instance_id, prepare.as_mut())
                        .await
                        .unwrap();
                    Some(expected_version)
                }
                RowState::Missing => {
                    delete(instance_id, prepare.as_mut()).await.unwrap();
                    None
                }
                RowState::LiveWithNewerVersion => {
                    let newer_version = expected_version.increment();
                    sqlx::query("UPDATE instances SET config_version = $1 WHERE id = $2")
                        .bind(newer_version)
                        .bind(instance_id)
                        .execute(prepare.as_mut())
                        .await
                        .unwrap();
                    Some(newer_version)
                }
            };
            prepare.commit().await.unwrap();

            let mut update = pool.begin().await.unwrap();
            let error = match stale_update {
                StaleUpdate::Config => {
                    update_config(
                        update.as_mut(),
                        instance_id,
                        expected_version,
                        stale_snapshot.config,
                        stale_snapshot.metadata,
                    )
                    .await
                }
                StaleUpdate::OperatingSystem => {
                    update_os(
                        update.as_mut(),
                        instance_id,
                        expected_version,
                        stale_snapshot.config.os,
                    )
                    .await
                }
            }
            .expect_err("a stale writer must not update the instance");

            match row_state {
                RowState::Deleted => {
                    assert!(matches!(&error, DatabaseError::FailedPrecondition(_)));
                    assert_eq!(
                        error.to_string(),
                        format!("instance {instance_id} is being deleted"),
                        "unexpected error for {case_name}",
                    );
                }
                RowState::Missing => {
                    assert!(matches!(&error, DatabaseError::FailedPrecondition(_)));
                    assert_eq!(
                        error.to_string(),
                        format!("instance {instance_id} does not exist"),
                        "unexpected error for {case_name}",
                    );
                }
                RowState::LiveWithNewerVersion => assert!(
                    matches!(
                        &error,
                        DatabaseError::ConcurrentModificationError("instance", version)
                            if version == &expected_version.to_string()
                    ),
                    "unexpected error for {case_name}: {error:?}",
                ),
            }
            update.rollback().await.unwrap();

            let version_after_rejection: Option<ConfigVersion> =
                sqlx::query_scalar("SELECT config_version FROM instances WHERE id = $1")
                    .bind(instance_id)
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                version_after_rejection, persisted_version,
                "the rejected {case_name} update changed the instance version",
            );
        }
    }

    #[crate::sqlx_test]
    async fn live_machine_lookup_locks_the_instance_row(pool: sqlx::PgPool) {
        let machine_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x48; 32],
            MachineType::Host,
        );
        let unassigned_machine_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x49; 32],
            MachineType::Host,
        );
        let mut setup = pool.begin().await.unwrap();
        let instance_id = seed_instance(&mut setup, 0x48, None).await;
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(unassigned_machine_id)
            .execute(setup.as_mut())
            .await
            .unwrap();
        setup.commit().await.unwrap();

        let mut reader = pool.begin().await.unwrap();
        assert!(
            find_live_by_machine_id_for_update(reader.as_mut(), &unassigned_machine_id)
                .await
                .unwrap()
                .is_none(),
            "an unassigned machine must return no instance",
        );
        let instance = find_live_by_machine_id_for_update(reader.as_mut(), &machine_id)
            .await
            .unwrap()
            .expect("the machine should have an assigned instance");
        assert_eq!(instance.id, instance_id);

        let mut deletion = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '100ms'")
            .execute(deletion.as_mut())
            .await
            .unwrap();
        let error = sqlx::query("UPDATE instances SET deleted = NOW() WHERE id = $1")
            .bind(instance_id)
            .execute(deletion.as_mut())
            .await
            .expect_err("deletion must wait for the instance reader");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("55P03"),
        );
        deletion.rollback().await.unwrap();
        reader.rollback().await.unwrap();

        let mut deletion = pool.begin().await.unwrap();
        mark_as_deleted(instance_id, deletion.as_mut())
            .await
            .unwrap();
        deletion.commit().await.unwrap();

        let mut reader = pool.begin().await.unwrap();
        let error = find_live_by_machine_id_for_update(reader.as_mut(), &machine_id)
            .await
            .expect_err("a deleted instance must not be returned for writes");
        assert_eq!(
            error.to_string(),
            format!("instance {instance_id} is being deleted"),
        );
        reader.rollback().await.unwrap();
    }

    /// A soft-deleted instance retains its network resources until physical
    /// deletion completes the asynchronous termination workflow.
    #[crate::sqlx_test]
    async fn network_reference_counts_retain_soft_deleted_instances(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let instance_id = seed_instance(&mut txn, 0x44, None).await;
        let segment_id = NetworkSegmentId::new();
        let vpc_id = VpcId::new();
        sqlx::query(
            "UPDATE instances SET network_config = jsonb_build_object( \
                 'interfaces', jsonb_build_array(jsonb_build_object( \
                     'network_segment_id', $2::text, 'vpc_id', $3::text))) \
             WHERE id = $1",
        )
        .bind(instance_id)
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        assert_eq!(
            count_network_segment_references(txn.as_mut(), &segment_id)
                .await
                .unwrap(),
            1,
        );
        assert_eq!(
            count_vpc_references(txn.as_mut(), &vpc_id).await.unwrap(),
            1,
        );

        mark_as_deleted(instance_id, txn.as_mut()).await.unwrap();
        assert_eq!(
            count_network_segment_references(txn.as_mut(), &segment_id)
                .await
                .unwrap(),
            1,
        );
        assert_eq!(
            count_vpc_references(txn.as_mut(), &vpc_id).await.unwrap(),
            1,
        );
        assert!(
            crate::instance_address::segment_has_allocations(txn.as_mut(), &segment_id)
                .await
                .unwrap()
        );

        delete(instance_id, txn.as_mut()).await.unwrap();
        assert_eq!(
            count_network_segment_references(txn.as_mut(), &segment_id)
                .await
                .unwrap(),
            0,
        );
        assert_eq!(
            count_vpc_references(txn.as_mut(), &vpc_id).await.unwrap(),
            0,
        );
        assert!(
            !crate::instance_address::segment_has_allocations(txn.as_mut(), &segment_id)
                .await
                .unwrap()
        );
    }

    /// VPC ownership can remain only in a segment relation or unresolved
    /// automatic intent. Pending updates keep current, old, and new configs
    /// live, but one instance still contributes only one reference.
    #[crate::sqlx_test]
    async fn count_vpc_references_covers_every_config_location(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let vpc_id = VpcId::new();
        let segment_id = NetworkSegmentId::new();
        sqlx::query(
            "INSERT INTO vpcs (id, name, version) \
             VALUES ($1, 'network-reference-vpc', 'V1-T0')",
        )
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id) \
             VALUES ($1, 'network-reference-segment', 'V1-T0', $2)",
        )
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        let segment_owned_instance = seed_instance(&mut txn, 0x45, None).await;
        sqlx::query(
            "UPDATE instances SET network_config = jsonb_build_object( \
                 'interfaces', jsonb_build_array(jsonb_build_object( \
                     'network_segment_id', $2::text))) \
             WHERE id = $1",
        )
        .bind(segment_owned_instance)
        .bind(segment_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        let automatic_instance = seed_instance(&mut txn, 0x46, None).await;
        sqlx::query(
            "UPDATE instances SET network_config = jsonb_build_object( \
                 'interfaces', jsonb_build_array(), \
                 'auto_config', jsonb_build_object('vpc_id', $2::text)) \
             WHERE id = $1",
        )
        .bind(automatic_instance)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        let pending_instance = seed_instance(&mut txn, 0x47, None).await;
        sqlx::query(
            "UPDATE instances SET \
                 network_config = jsonb_build_object( \
                     'interfaces', jsonb_build_array(jsonb_build_object( \
                         'vpc_id', $2::text))), \
                 update_network_config_request = jsonb_build_object( \
                     'old_config', jsonb_build_object( \
                         'interfaces', jsonb_build_array(jsonb_build_object( \
                             'network_segment_id', $3::text))), \
                     'new_config', jsonb_build_object( \
                         'interfaces', jsonb_build_array(), \
                         'auto_config', jsonb_build_object('vpc_id', $2::text))) \
             WHERE id = $1",
        )
        .bind(pending_instance)
        .bind(vpc_id)
        .bind(segment_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        assert_eq!(
            count_vpc_references(txn.as_mut(), &vpc_id).await.unwrap(),
            3,
            "each instance must be counted once regardless of where the reference appears",
        );
        assert_eq!(
            count_vpc_references(txn.as_mut(), &VpcId::new())
                .await
                .unwrap(),
            0,
            "unrelated VPCs must not match nested configuration fields",
        );
    }
}

#[cfg(test)]
mod count_ids_tests {
    use super::*;

    /// Seeds `n` instances for one tenant + instance type and returns the
    /// filter that selects exactly those instances.
    async fn seed_instances(
        txn: &mut PgConnection,
        tenant_org: &str,
        instance_type_id: &str,
        n: usize,
    ) -> model::instance::InstanceSearchFilter {
        sqlx::query("INSERT INTO instance_types (id, name) VALUES ($1, $1) ON CONFLICT DO NOTHING")
            .bind(instance_type_id)
            .execute(&mut *txn)
            .await
            .expect("seed instance_type");

        for _ in 0..n {
            let machine_id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO machines (id, dpf) \
                 VALUES ($1, '{\"enabled\": false, \"used_for_ingestion\": false}'::jsonb)",
            )
            .bind(machine_id)
            .execute(&mut *txn)
            .await
            .expect("seed machine");
            sqlx::query(
                "INSERT INTO instances (id, machine_id, tenant_org, instance_type_id) \
                 VALUES (gen_random_uuid(), $1, $2, $3)",
            )
            .bind(machine_id)
            .bind(tenant_org)
            .bind(instance_type_id)
            .execute(&mut *txn)
            .await
            .expect("seed instance");
        }

        model::instance::InstanceSearchFilter {
            label: None,
            tenant_org_id: Some(tenant_org.to_string()),
            vpc_id: None,
            instance_type_id: Some(instance_type_id.to_string()),
        }
    }

    /// `count_ids` returns the same tally as `find_ids(..).len()`, but the win
    /// is in what crosses the wire: `find_ids` materializes and decodes one
    /// `InstanceId` per matching row (N rows), whereas `count_ids` returns a
    /// single scalar (1 row). Same one query either way — this is a
    /// rows-transferred/decoded win (N -> 1), not a round-trip win.
    #[crate::sqlx_test]
    async fn count_ids_matches_find_ids_len_without_materializing(pool: sqlx::PgPool) {
        const N: usize = 5;
        let mut txn = pool.begin().await.unwrap();
        let filter = seed_instances(&mut txn, "count-ids-tenant", "count-ids-type", N).await;

        let ids = find_ids(&mut *txn, filter.clone()).await.unwrap();
        let count = count_ids(&mut *txn, filter).await.unwrap();

        // find_ids materialized N ids; count_ids returned the scalar tally.
        assert_eq!(ids.len(), N, "find_ids should materialize {N} ids");
        assert_eq!(count, N as i64, "count_ids should tally {N}");
        assert_eq!(
            count,
            ids.len() as i64,
            "count_ids must agree with find_ids(..).len()"
        );
    }

    /// The shared WHERE builder keeps `count_ids` scoped to the same filter as
    /// `find_ids`: instances for a different tenant are not counted.
    #[crate::sqlx_test]
    async fn count_ids_respects_the_filter(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let filter = seed_instances(&mut txn, "tenant-a", "type-a", 3).await;
        // Rows sharing only ONE filter dimension: same tenant with another
        // type, and another tenant with the same type. The filter must exclude
        // both, so a regression that drops either predicate fails the count.
        let _ = seed_instances(&mut txn, "tenant-a", "type-b", 2).await;
        let _ = seed_instances(&mut txn, "tenant-b", "type-a", 2).await;

        let count = count_ids(&mut *txn, filter).await.unwrap();
        assert_eq!(count, 3, "only tenant-a/type-a instances are counted");
    }
}
