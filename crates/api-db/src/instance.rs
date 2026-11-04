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
use std::collections::HashSet;
use std::ops::DerefMut;
use std::str::FromStr;

use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineId};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use carbide_uuid::vpc::VpcId;
use chrono::prelude::*;
use config_version::ConfigVersion;
use model::instance::NewInstance;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::{
    InstanceNetworkConfig, InstanceNetworkConfigUpdate, InstanceServiceInterfaceConfig,
};
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::snapshot::{self, InstanceSnapshot, InstanceSnapshotPgJson};
use model::metadata::Metadata;
use model::network_segment::NetworkSegmentSearchConfig;
use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
use model::tenant::TenantOrganizationId;
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::db_read::DbReader;
use crate::operating_system::{self, OperatingSystem as OsRow};
use crate::{
    BIND_LIMIT, ColumnInfo, ConditionalWrite, DatabaseError, DatabaseResult,
    FilterableQueryBuilder, ObjectColumnFilter, instance_address,
};

pub mod nic_firmware;

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

/// Adds the rows for every host configuration that can still own network
/// resources for an instance. The current value is in `network_config`; both
/// configurations in `update_network_config_request` can still own host
/// resources until the update releases the old ones.
///
/// A SLAAC interface can retain a prefix without creating a row in
/// `instance_addresses`, so searches and deletion checks cannot use address
/// rows as their only source. Callers must name the outer table `instances`
/// because this expression refers to it directly.
fn push_host_network_config_rows(builder: &mut sqlx::QueryBuilder<sqlx::Postgres>) {
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
        "(
        EXISTS (
            SELECT 1
            FROM ",
    );
    push_host_network_config_rows(builder);
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
        )
        OR EXISTS (
            SELECT 1
            FROM jsonb_array_elements(
                COALESCE(instances.network_config->'service_interfaces', '[]'::jsonb)
            ) AS service_interfaces(service_interface)
            WHERE (service_interfaces.service_interface->>'network_segment_id')::uuid = ",
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
    push_host_network_config_rows(builder);
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
            OR EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    COALESCE(instances.network_config->'service_interfaces', '[]'::jsonb)
                ) AS service_interfaces(service_interface)
                WHERE (service_interfaces.service_interface->>'vpc_id')::uuid = target.vpc_id
                OR EXISTS (
                    SELECT 1
                    FROM network_segments
                    WHERE id =
                        (service_interfaces.service_interface->>'network_segment_id')::uuid
                      AND vpc_id = target.vpc_id
                )
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
    /// Current extension-service attachment configuration used by locked lifecycle updates.
    #[sqlx(json)]
    pub extension_services_config: InstanceExtensionServicesConfig,
    /// Current `extension_services_config_version`.
    pub extension_services_config_version: ConfigVersion,
    /// Current host and service network configuration used by locked lifecycle updates.
    #[sqlx(json)]
    pub network_config: InstanceNetworkConfig,
    /// Current `network_config_version`.
    pub network_config_version: ConfigVersion,
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
            i.extension_services_config,
            i.extension_services_config_version,
            i.network_config,
            i.network_config_version,
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

/// Returns the current DPU/VPC memberships owned by service-interface records.
///
/// Current endpoint records, including terminating tombstones, are the durable
/// release authority. Historical service collections in a pending host-network
/// update are excluded because they cannot establish current service ownership.
pub async fn find_service_vpc_memberships(
    txn: impl DbReader<'_>,
    instance_id: InstanceId,
) -> Result<Vec<(DpuMachineId, VpcId)>, DatabaseError> {
    let query = "SELECT DISTINCT
            service_interface->>'dpu_id' AS dpu_id,
            (service_interface->>'vpc_id')::uuid AS vpc_id
        FROM instances
        CROSS JOIN LATERAL jsonb_array_elements(
            COALESCE(network_config->'service_interfaces', '[]'::jsonb)
        ) AS service_interfaces(service_interface)
        WHERE id = $1
        ORDER BY dpu_id, vpc_id";
    sqlx::query_as(query)
        .bind(instance_id)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Returns the highest service-VPC slot referenced by any attachment for a service so registration
/// reconciliation keeps slot-specific DPUServiceInterface and DPUServiceChain resources in use.
pub async fn highest_retained_service_vpc_slot(
    txn: impl DbReader<'_>,
    service_id: ExtensionServiceId,
) -> Result<Option<u32>, DatabaseError> {
    let query = r#"
        SELECT MAX((service_interface->>'slot_index')::bigint)
        FROM instances
        CROSS JOIN LATERAL jsonb_array_elements(
            COALESCE(extension_services_config->'service_configs', '[]'::jsonb)
        ) AS attachments(attachment)
        CROSS JOIN LATERAL jsonb_array_elements(
            COALESCE(network_config->'service_interfaces', '[]'::jsonb)
        ) AS service_interfaces(service_interface)
        WHERE attachment->>'service_id' = $1
          AND attachment->>'id' = service_interface->>'attachment_id'
    "#;
    let highest: Option<i64> = sqlx::query_scalar(query)
        .bind(service_id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    highest
        .map(|slot| {
            u32::try_from(slot).map_err(|error| {
                DatabaseError::InvalidArgument(format!(
                    "stored service-VPC slot {slot} for service {service_id} is invalid: {error}"
                ))
            })
        })
        .transpose()
}

/// Finds every instance retaining an attachment to the selected service.
///
/// Optional version selection supports legacy KubernetesPod attachments and
/// will be removed when attachment versions are retired; DPF Helm chart and
/// service VPC attachments use only V1.
///
/// Soft-deleted instances remain results because their attachment cleanup is
/// still outstanding until physical instance deletion completes.
pub async fn find_by_extension_service(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    version: Option<ConfigVersion>,
) -> Result<Vec<InstanceSnapshot>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new(
        r#"SELECT row_to_json(i.*) FROM instances i
            WHERE EXISTS (
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

/// Counts instances whose current service endpoints or current/pending host
/// configuration reference `segment_id`.
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

/// Counts instances whose current service endpoints or current/pending host
/// configuration refer to `vpc_id`.
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

/// Stages a host-network replacement without copying service endpoints, whose
/// ownership and lifetime remain in the live instance configuration.
pub async fn trigger_update_network_config_request(
    instance_id: &InstanceId,
    current: &InstanceNetworkConfig,
    requested: &InstanceNetworkConfig,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), DatabaseError> {
    let mut network_config_request = InstanceNetworkConfigUpdate {
        old_config: current.clone(),
        new_config: requested.clone(),
    };
    // Pending host snapshots must never restore or release service-owned resources.
    network_config_request.old_config.service_interfaces.clear();
    network_config_request.new_config.service_interfaces.clear();
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

/// `InstanceExtensionServicesNotCurrent` means the Instance is missing, its
/// `extension_services_config_version` changed, or its `extension_services_config` changed.
/// The conditional write does not distinguish these cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceExtensionServicesNotCurrent;

/// `InstanceServiceNetworkNotCurrent` means the Instance row is absent or an expected
/// `extension_services_config_version`, `extension_services_config`, `network_config_version`, or
/// `network_config` value no longer matches; neither configuration was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceServiceNetworkNotCurrent;

/// Reports the two configuration versions written by one service-VPC configuration change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceServiceNetworkVersions {
    /// `extension_services_config_version` after the conditional write.
    pub extension_services: ConfigVersion,
    /// `network_config_version` after the same conditional write.
    pub network: ConfigVersion,
}

/// Replaces `extension_services_config` and `network_config` in one conditional statement so readers
/// never observe only half of an activation or termination request.
///
/// The write requires a live Instance whose expected configuration versions,
/// `extension_services_config`, and `network_config` still match.
/// `increment_versions` advances both configuration versions when true and preserves them when false.
#[allow(clippy::too_many_arguments)]
pub async fn update_service_network_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_extension_version: ConfigVersion,
    expected_extension_config: &InstanceExtensionServicesConfig,
    new_extension_config: &InstanceExtensionServicesConfig,
    expected_network_version: ConfigVersion,
    expected_network_config: &InstanceNetworkConfig,
    new_network_config: &InstanceNetworkConfig,
    increment_versions: bool,
) -> Result<
    ConditionalWrite<InstanceServiceNetworkVersions, InstanceServiceNetworkNotCurrent>,
    DatabaseError,
> {
    update_service_network_config_inner(
        txn,
        instance_id,
        expected_extension_version,
        expected_extension_config,
        new_extension_config,
        expected_network_version,
        expected_network_config,
        new_network_config,
        increment_versions,
        true,
    )
    .await
}

/// Conditionally writes both configurations while allowing observed cleanup to finish on a
/// soft-deleted instance without permitting user-requested mutations after release begins.
#[allow(clippy::too_many_arguments)]
async fn update_service_network_config_inner(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_extension_version: ConfigVersion,
    expected_extension_config: &InstanceExtensionServicesConfig,
    new_extension_config: &InstanceExtensionServicesConfig,
    expected_network_version: ConfigVersion,
    expected_network_config: &InstanceNetworkConfig,
    new_network_config: &InstanceNetworkConfig,
    increment_versions: bool,
    require_live: bool,
) -> Result<
    ConditionalWrite<InstanceServiceNetworkVersions, InstanceServiceNetworkNotCurrent>,
    DatabaseError,
> {
    let next_extension_version = if increment_versions {
        expected_extension_version.increment()
    } else {
        expected_extension_version
    };
    let next_network_version = if increment_versions {
        expected_network_version.increment()
    } else {
        expected_network_version
    };

    let snapshot_query = r#"
        SELECT extension_services_config_version,
            extension_services_config,
            network_config_version,
            network_config
        FROM instances
        WHERE id=$1
    "#;
    let snapshot: Option<(
        ConfigVersion,
        Json<serde_json::Value>,
        ConfigVersion,
        Json<serde_json::Value>,
    )> = sqlx::query_as(snapshot_query)
        .bind(instance_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(snapshot_query, error))?;

    let Some((
        current_extension_version,
        Json(raw_extension_config),
        current_network_version,
        Json(raw_network_config),
    )) = snapshot
    else {
        if require_live {
            ensure_live_for_config_update(txn, instance_id).await?;
        }
        return Ok(ConditionalWrite::NotApplied(
            InstanceServiceNetworkNotCurrent,
        ));
    };

    // Legacy auto=false represents explicit networking only when auto_config is absent or null.
    let legacy_auto_is_compatible = match raw_network_config.get("auto") {
        None => true,
        Some(serde_json::Value::Bool(false)) => matches!(
            raw_network_config.get("auto_config"),
            None | Some(serde_json::Value::Null)
        ),
        Some(_) => false,
    };
    let current_extension_config =
        serde_json::from_value::<InstanceExtensionServicesConfig>(raw_extension_config.clone());
    let current_network_config =
        serde_json::from_value::<InstanceNetworkConfig>(raw_network_config.clone());
    let snapshot_matches = current_extension_version == expected_extension_version
        && current_network_version == expected_network_version
        && legacy_auto_is_compatible
        && current_extension_config.as_ref().ok() == Some(expected_extension_config)
        && current_network_config.as_ref().ok() == Some(expected_network_config);
    if !snapshot_matches {
        if require_live {
            ensure_live_for_config_update(txn, instance_id).await?;
        }
        return Ok(ConditionalWrite::NotApplied(
            InstanceServiceNetworkNotCurrent,
        ));
    }

    // Compare the exact JSON values read above so cleanup cannot be overwritten after preserving
    // `extension_services_config_version` and `network_config_version`.
    //
    // TODO: Consider adding `service_network_version` so these predicates do not compare JSON.
    // This requires a migration and changes to every network and extension-service writer,
    // including instances without service-VPC endpoints, so keep this uncommon path self-contained.
    let query = r#"
        UPDATE instances
        SET extension_services_config_version=$1,
            extension_services_config=$2::jsonb,
            network_config_version=$3,
            network_config=$4::jsonb
        WHERE id=$5
          AND extension_services_config_version=$6
          AND network_config_version=$7
          AND (NOT $10 OR deleted IS NULL)
          AND extension_services_config=$8::jsonb
          AND network_config=$9::jsonb
        RETURNING id
    "#;
    let updated_id: Option<InstanceId> = sqlx::query_scalar(query)
        .bind(next_extension_version)
        .bind(Json(new_extension_config))
        .bind(next_network_version)
        .bind(Json(new_network_config))
        .bind(instance_id)
        .bind(current_extension_version)
        .bind(current_network_version)
        .bind(Json(&raw_extension_config))
        .bind(Json(&raw_network_config))
        .bind(require_live)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    if updated_id.is_none() && require_live {
        ensure_live_for_config_update(txn, instance_id).await?;
    }

    Ok(match updated_id {
        Some(_) => ConditionalWrite::Applied(InstanceServiceNetworkVersions {
            extension_services: next_extension_version,
            network: next_network_version,
        }),
        None => ConditionalWrite::NotApplied(InstanceServiceNetworkNotCurrent),
    })
}

/// Reports that reclamation did not apply because the Instance is absent, either configuration
/// version changed, the attachment is not terminating, or its persisted service interfaces are
/// empty or differ from those expected; no attachment, service-interface, address, or segment
/// changes occur in these cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceVpcReclamationNotCurrent;

/// Supplies the post-cleanup host memberships needed to release a final shared DPU/VPC loopback.
/// The caller must derive these memberships from the same network configuration version passed to
/// reclamation.
pub struct ServiceVpcLoopbackReleaseContext<'a> {
    /// Resource pools containing the loopback allocation released with the database row.
    pub common_pools: &'a model::resource_pool::common::CommonPools,
    /// Host memberships that still require exact DPU/VPC loopbacks after endpoint removal.
    pub retained_host_memberships: &'a HashSet<(DpuMachineId, VpcId)>,
}

/// Reclaims one nonempty terminating attachment only when its extension-service and network
/// configuration versions and exact persisted service-interface set still match; callers must validate
/// current external cleanup observations in the same transaction before committing an applied result.
///
/// `loopback_release = None` leaves shared DPU/VPC loopbacks unchanged; `Some` releases a candidate
/// only when no remaining service interface or supplied host membership still requires it.
pub async fn reclaim_service_vpc_attachment(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    attachment_id: uuid::Uuid,
    expected_extension_version: ConfigVersion,
    expected_network_version: ConfigVersion,
    expected_endpoints: &[InstanceServiceInterfaceConfig],
    loopback_release: Option<ServiceVpcLoopbackReleaseContext<'_>>,
) -> Result<ConditionalWrite<(), ServiceVpcReclamationNotCurrent>, DatabaseError> {
    // Address rows precede the Instance row in every deletion path, matching concurrent allocation.
    instance_address::lock_addresses_for_instance(&mut *txn, instance_id).await?;
    let Some(locked) = find_by_id_for_update(&mut *txn, instance_id).await? else {
        return Ok(ConditionalWrite::NotApplied(
            ServiceVpcReclamationNotCurrent,
        ));
    };
    if locked.extension_services_config_version != expected_extension_version
        || locked.network_config_version != expected_network_version
    {
        return Ok(ConditionalWrite::NotApplied(
            ServiceVpcReclamationNotCurrent,
        ));
    }

    let terminating = locked
        .extension_services_config
        .service_configs
        .iter()
        .any(|attachment| attachment.id == Some(attachment_id) && attachment.removed.is_some());
    let mut attachment_endpoints = locked
        .network_config
        .service_interfaces
        .iter()
        .filter(|endpoint| endpoint.attachment_id == attachment_id)
        .cloned()
        .collect::<Vec<_>>();
    let mut expected_attachment_endpoints = expected_endpoints.to_vec();
    attachment_endpoints.sort_by_key(|endpoint| {
        (
            endpoint.dpu_id,
            endpoint.interface_ordinal,
            endpoint.internal_uuid,
        )
    });
    expected_attachment_endpoints.sort_by_key(|endpoint| {
        (
            endpoint.dpu_id,
            endpoint.interface_ordinal,
            endpoint.internal_uuid,
        )
    });
    if !terminating
        || attachment_endpoints.is_empty()
        || attachment_endpoints != expected_attachment_endpoints
    {
        return Ok(ConditionalWrite::NotApplied(
            ServiceVpcReclamationNotCurrent,
        ));
    }

    // Derive normalized addresses from the attachment's persisted service-interface link prefixes.
    let addresses = attachment_endpoints
        .iter()
        .map(|endpoint| {
            instance_address::service_address_for_prefix(endpoint.link_prefix)
                .map(|address| (endpoint.network_segment_id, address))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let candidate_memberships = attachment_endpoints
        .iter()
        .map(|endpoint| (endpoint.dpu_id, endpoint.vpc_id))
        .collect::<HashSet<_>>();
    let mut new_extension_config = locked.extension_services_config.clone();
    new_extension_config
        .service_configs
        .retain(|attachment| attachment.id != Some(attachment_id));
    let mut new_network_config = locked.network_config.clone();
    new_network_config
        .service_interfaces
        .retain(|endpoint| endpoint.attachment_id != attachment_id);
    // Conditionally remove the attachment and service interfaces before deleting their Instance
    // addresses, network segments, or DPU/VPC loopbacks so `ConditionalWrite::NotApplied` leaves
    // those rows unchanged.
    match update_service_network_config_inner(
        &mut *txn,
        instance_id,
        expected_extension_version,
        &locked.extension_services_config,
        &new_extension_config,
        expected_network_version,
        &locked.network_config,
        &new_network_config,
        false,
        false,
    )
    .await?
    {
        ConditionalWrite::Applied(_) => {}
        ConditionalWrite::NotApplied(_) => {
            return Ok(ConditionalWrite::NotApplied(
                ServiceVpcReclamationNotCurrent,
            ));
        }
    }

    // Delete normalized addresses only after the Instance configuration no longer references them.
    instance_address::delete_addresses_for_instance(&mut *txn, instance_id, &addresses).await?;

    // Mark generated network segments deleted only after `extension_services_config`,
    // `network_config.service_interfaces`, and normalized addresses no longer reference them.
    for segment_id in attachment_endpoints
        .iter()
        .map(|endpoint| endpoint.network_segment_id)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let Some(segment) = crate::network_segment::find_by(
            &mut *txn,
            ObjectColumnFilter::One(crate::network_segment::IdColumn, &segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?
        .pop() else {
            return Err(DatabaseError::FailedPrecondition(format!(
                "service-VPC network segment {segment_id} does not exist"
            )));
        };
        crate::network_segment::mark_as_deleted(&segment, &mut *txn).await?;
    }

    if let Some(loopback_release) = loopback_release {
        crate::vpc_dpu_loopback::release_unretained_instance_loopbacks(
            loopback_release.common_pools,
            &mut *txn,
            instance_id,
            &candidate_memberships,
            loopback_release.retained_host_memberships,
        )
        .await?;
    }

    Ok(ConditionalWrite::Applied(()))
}

/// `update_extension_services_config` replaces attachments only when both
/// their generation and content match the caller's snapshot.
///
/// `increment_version` advances the generation for a requested configuration
/// change. Controller cleanup leaves it unchanged because it only removes
/// attachments whose termination was already observed. Comparing
/// `expected_config` also rejects snapshots taken before cleanup, without
/// requesting another configuration generation.
///
/// Missing or changed snapshots return `NotApplied`; database failures return
/// `Err`. The caller owns the surrounding transaction.
///
/// Legacy attachment IDs represented by an explicit JSON null compare the
/// same as omitted IDs. Present IDs and all other content remain part of the
/// comparison.
pub async fn update_extension_services_config(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    expected_version: ConfigVersion,
    expected_config: &InstanceExtensionServicesConfig,
    new_config: &InstanceExtensionServicesConfig,
    increment_version: bool,
) -> Result<ConditionalWrite<(), InstanceExtensionServicesNotCurrent>, DatabaseError> {
    let next_version = if increment_version {
        expected_version.increment()
    } else {
        expected_version
    };

    let query = r#"
        UPDATE instances
        SET extension_services_config_version=$1,
            extension_services_config=$2::jsonb
        WHERE id=$3
          AND extension_services_config_version=$4
          -- Current writers omit absent IDs. Remove only legacy null IDs before
          -- comparing, while retaining every other field and the attachment order.
          AND jsonb_set(
                extension_services_config,
                '{service_configs}',
                (
                    SELECT COALESCE(
                        jsonb_agg(
                            CASE
                                WHEN service_config->'id' = 'null'::jsonb
                                    THEN service_config - 'id'
                                ELSE service_config
                            END
                            ORDER BY ordinal
                        ),
                        '[]'::jsonb
                    )
                    FROM jsonb_array_elements(
                        extension_services_config->'service_configs'
                    ) WITH ORDINALITY AS services(service_config, ordinal)
                )
              ) = $5::jsonb
        RETURNING id
    "#;
    let updated_id: Option<InstanceId> = sqlx::query_scalar(query)
        .bind(next_version)
        .bind(sqlx::types::Json(new_config))
        .bind(instance_id)
        .bind(expected_version)
        .bind(sqlx::types::Json(expected_config))
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    Ok(match updated_id {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(InstanceExtensionServicesNotCurrent),
    })
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

/// Deletes an instance and all normalized address allocations it owns.
///
/// Address rows are locked before the instance row to preserve the allocation
/// lock order. Callers must not enter while holding the instance lock. Current
/// service-VPC memberships return [`DatabaseError::FailedPrecondition`] without
/// deleting either the instance or its addresses.
pub async fn delete(instance_id: InstanceId, txn: &mut PgConnection) -> DatabaseResult<()> {
    delete_instance_records(instance_id, txn, true).await
}

/// Deletes an Instance and its normalized addresses despite retained service memberships after
/// the caller obtains explicit administrative consent to orphan DPF state.
///
/// Address rows remain locked before the Instance row so allocation and deletion preserve the
/// established lock order.
pub async fn force_delete(instance_id: InstanceId, txn: &mut PgConnection) -> DatabaseResult<()> {
    delete_instance_records(instance_id, txn, false).await
}

/// Locks normalized address rows before the Instance row and deletes those address rows plus the
/// Instance row while optionally rejecting retained service memberships to preserve serialization.
async fn delete_instance_records(
    instance_id: InstanceId,
    txn: &mut PgConnection,
    require_empty_service_memberships: bool,
) -> DatabaseResult<()> {
    // Preserve the address-before-instance lock order used by allocation and
    // partial host cleanup, then prevent endpoint creation while checking.
    instance_address::lock_addresses_for_instance(&mut *txn, instance_id).await?;
    let _locked_instance = find_by_id_for_update(&mut *txn, instance_id).await?;

    if require_empty_service_memberships {
        let service_memberships = find_service_vpc_memberships(&mut *txn, instance_id).await?;
        if !service_memberships.is_empty() {
            return Err(DatabaseError::FailedPrecondition(format!(
                "instance {instance_id} cannot be deleted while {} service VPC memberships remain",
                service_memberships.len(),
            )));
        }
    }

    instance_address::delete(&mut *txn, instance_id).await?;

    let query = "DELETE FROM instances where id=$1::uuid RETURNING id";
    sqlx::query_as::<_, InstanceId>(query)
        .bind(instance_id)
        .fetch_one(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// Marks an instance for release and clears only its `nic_firmware_snapshot`
/// selection context. This does not delete a firmware operation or its recovery state.
pub async fn mark_as_deleted(
    instance_id: InstanceId,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let query = "UPDATE instances SET deleted=NOW(), nic_firmware_snapshot=NULL WHERE id=$1::uuid RETURNING id";

    let _id = sqlx::query_as::<_, InstanceId>(query)
        .bind(instance_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::net::IpAddr;
    use std::sync::Arc;

    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use carbide_uuid::network::NetworkPrefixId;
    use carbide_uuid::vpc::VpcPrefixId;
    use mac_address::MacAddress;
    use model::extension_service::DpuTarget;
    use model::instance::config::extension_services::InstanceExtensionServiceConfig;
    use model::instance::config::network::{
        InstanceInterfaceConfig, InstanceNetworkAutoConfig, InstanceServiceInterfaceConfig,
        InterfaceFunctionId,
    };
    use model::network_segment::NetworkSegmentSearchConfig;
    use model::resource_pool::common::{
        CommonPools, FNN_ASN, LOOPBACK_IP, VLANID, VNI, VPC_DPU_LOOPBACK, VPC_VNI,
    };
    use model::resource_pool::{
        OwnerType, Range, ResourcePoolDef, ResourcePoolEntryState, ResourcePoolType,
    };

    use super::*;
    use crate::test_support::postgres::wait_for_blocked_query;

    const INSTANCE_IPXE_SCRIPT: &str = "#!ipxe boot-from-instance-columns";

    /// Creates the common pools required to prove service reclamation updates loopback rows and
    /// pool allocation state atomically rather than validating only the membership predicate.
    async fn service_vpc_loopback_pools(pool: &sqlx::PgPool) -> Arc<CommonPools> {
        let integer_pool = || ResourcePoolDef {
            ranges: vec![Range {
                start: "1".to_string(),
                end: "16".to_string(),
                auto_assign: true,
            }],
            prefix: None,
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        };
        let address_pool = |prefix: &str| ResourcePoolDef {
            ranges: Vec::new(),
            prefix: Some(prefix.to_string()),
            pool_type: ResourcePoolType::Ipv4,
            delegate_prefix_len: None,
        };
        let definitions = HashMap::from([
            (LOOPBACK_IP.to_string(), address_pool("192.0.2.0/29")),
            (VLANID.to_string(), integer_pool()),
            (VNI.to_string(), integer_pool()),
            (VPC_VNI.to_string(), integer_pool()),
            (FNN_ASN.to_string(), integer_pool()),
            (VPC_DPU_LOOPBACK.to_string(), address_pool("203.0.113.0/29")),
        ]);
        let mut txn = pool.begin().await.expect("begin loopback pool setup");
        crate::resource_pool::define_all_from(txn.as_mut(), &definitions)
            .await
            .expect("define loopback test pools");
        txn.commit().await.expect("commit loopback pool setup");
        crate::resource_pool::create_common_pools(pool.clone(), HashSet::new())
            .await
            .expect("create common loopback pools")
    }

    struct ServiceVpcReclamationFixture {
        instance_id: InstanceId,
        attachment_id: uuid::Uuid,
        survivor_attachment_id: uuid::Uuid,
        dpu_id: DpuMachineId,
        vpc_id: VpcId,
        segment_id: NetworkSegmentId,
        survivor_segment_id: NetworkSegmentId,
        service_id: ExtensionServiceId,
        survivor_service_id: ExtensionServiceId,
        endpoint: InstanceServiceInterfaceConfig,
        survivor_endpoint: InstanceServiceInterfaceConfig,
        attachments: InstanceExtensionServicesConfig,
        network: InstanceNetworkConfig,
        baseline: InstanceSnapshot,
        target_loopback: IpAddr,
    }

    /// Seeds one terminating and one active service-VPC attachment with distinct addresses,
    /// segments, slots, and a target loopback so reclamation tests can distinguish exact cleanup
    /// from changes to the surviving attachment.
    async fn seed_service_vpc_reclamation_fixture(
        txn: &mut PgConnection,
        common_pools: &CommonPools,
    ) -> ServiceVpcReclamationFixture {
        // Create independent VPCs and network segments for the target and surviving attachments.
        let instance_id = seed_instance(txn, 0x4d, None).await;
        let vpc_id = VpcId::new();
        let survivor_vpc_id = VpcId::new();
        let segment_id = NetworkSegmentId::new();
        let survivor_segment_id = NetworkSegmentId::new();
        sqlx::query(
            "INSERT INTO vpcs (id, name, organization_id, version) \
             VALUES ($1, 'reclaim-service-vpc', 'service-reclamation', 'V1-T0')",
        )
        .bind(vpc_id)
        .execute(&mut *txn)
        .await
        .expect("seed reclamation VPC");
        sqlx::query(
            "INSERT INTO vpcs (id, name, organization_id, version) \
             VALUES ($1, 'surviving-service-vpc', 'service-reclamation', 'V1-T0')",
        )
        .bind(survivor_vpc_id)
        .execute(&mut *txn)
        .await
        .expect("seed surviving reclamation VPC");
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id) \
             VALUES ($1, 'reclaim-service-segment', 'V1-T0', $2)",
        )
        .bind(segment_id)
        .bind(vpc_id)
        .execute(&mut *txn)
        .await
        .expect("seed reclamation segment");
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id) \
             VALUES ($1, 'surviving-service-segment', 'V1-T0', $2)",
        )
        .bind(survivor_segment_id)
        .bind(survivor_vpc_id)
        .execute(&mut *txn)
        .await
        .expect("seed surviving service segment");

        // Allocate the target VPC/DPU loopback before persisting both service interfaces.
        let attachment_id = uuid::Uuid::new_v4();
        let survivor_attachment_id = uuid::Uuid::new_v4();
        let dpu_id: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x5d; 32],
            MachineType::Dpu,
        )
        .try_into()
        .expect("DPU machine ID");
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}')")
            .bind(dpu_id)
            .execute(&mut *txn)
            .await
            .expect("seed reclamation DPU");
        let target_loopback = crate::vpc_dpu_loopback::get_or_allocate_loopback_ip_for_vpc(
            common_pools,
            txn,
            &dpu_id,
            &vpc_id,
        )
        .await
        .expect("allocate target service loopback");
        let service_id = ExtensionServiceId::new();
        let survivor_service_id = ExtensionServiceId::new();
        let endpoint = InstanceServiceInterfaceConfig {
            attachment_id, // Selects the one terminating attachment eligible for cleanup.
            interface_ordinal: 0,
            dpu_id,
            slot_index: 0, // Drives the retained registration-capacity query.
            vpc_id,
            vpc_prefix_id: VpcPrefixId::new(),
            network_segment_id: segment_id, // Selects the exact address and generated segment.
            network_prefix_id: NetworkPrefixId::new(),
            // Derives the normalized address that reclamation must remove.
            link_prefix: "192.0.2.0/31".parse().expect("valid service link"),
            mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 2]),
            internal_uuid: uuid::Uuid::new_v4(), // Distinguishes the stored service interface from a mismatched expectation.
        };
        let survivor_endpoint = InstanceServiceInterfaceConfig {
            attachment_id: survivor_attachment_id,
            interface_ordinal: 0,
            dpu_id,
            slot_index: 1, // Keeps the surviving service above the target slot in capacity checks.
            vpc_id: survivor_vpc_id,
            vpc_prefix_id: VpcPrefixId::new(),
            network_segment_id: survivor_segment_id,
            network_prefix_id: NetworkPrefixId::new(),
            link_prefix: "198.51.100.0/31"
                .parse()
                .expect("valid surviving service link"),
            mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 3]),
            internal_uuid: uuid::Uuid::new_v4(),
        };
        let attachments = InstanceExtensionServicesConfig {
            service_configs: vec![
                InstanceExtensionServiceConfig {
                    id: Some(attachment_id),
                    service_id, // Connects this service interface to the retained-slot query.
                    version: ConfigVersion::initial(),
                    dpu_target: Some(DpuTarget::All),
                    removed: Some(Utc::now()), // Marks the target as terminating for reclamation.
                },
                InstanceExtensionServiceConfig {
                    id: Some(survivor_attachment_id),
                    service_id: survivor_service_id,
                    version: ConfigVersion::initial(),
                    dpu_target: Some(DpuTarget::All),
                    removed: None, // Keeps the survivor active and ineligible for reclamation.
                },
            ],
        };
        let mut network = InstanceNetworkConfig::default();
        network
            .service_interfaces
            .extend([endpoint.clone(), survivor_endpoint.clone()]);
        sqlx::query(
            "UPDATE instances SET extension_services_config=$2, network_config=$3 WHERE id=$1",
        )
        .bind(instance_id)
        .bind(Json(&attachments))
        .bind(Json(&network))
        .execute(&mut *txn)
        .await
        .expect("seed reclamation configurations");

        // Persist one normalized Instance address for each service interface.
        let service_address: IpAddr = "192.0.2.1".parse().expect("valid service address");
        sqlx::query(
            "INSERT INTO instance_addresses \
                (instance_id, address, prefix, segment_id, vpc_id) \
             VALUES ($1, $2, '192.0.2.0/31'::cidr, $3, $4)",
        )
        .bind(instance_id)
        .bind(service_address)
        .bind(segment_id)
        .bind(vpc_id)
        .execute(&mut *txn)
        .await
        .expect("seed normalized service address");
        sqlx::query(
            "INSERT INTO instance_addresses \
                (instance_id, address, prefix, segment_id, vpc_id) \
             VALUES ($1, '198.51.100.1'::inet, '198.51.100.0/31'::cidr, $2, $3)",
        )
        .bind(instance_id)
        .bind(survivor_segment_id)
        .bind(survivor_vpc_id)
        .execute(&mut *txn)
        .await
        .expect("seed surviving normalized service address");

        // Soft-delete the Instance so reclamation exercises cleanup after ordinary release.
        mark_as_deleted(instance_id, &mut *txn)
            .await
            .expect("soft-delete reclamation instance");
        let baseline = find_by_id(&mut *txn, instance_id)
            .await
            .expect("read reclamation baseline")
            .expect("seeded instance exists");

        ServiceVpcReclamationFixture {
            instance_id,
            attachment_id,
            survivor_attachment_id,
            dpu_id,
            vpc_id,
            segment_id,
            survivor_segment_id,
            service_id,
            survivor_service_id,
            endpoint,
            survivor_endpoint,
            attachments,
            network,
            baseline,
            target_loopback,
        }
    }

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
             VALUES ($1, $2, $3, '{\"interfaces\": [], \"auto_config\": null}'::jsonb, '{\"gpu_configs\": []}'::jsonb) \
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

    /// Verifies current writers can mark predecessor attachments for removal
    /// when their IDs were omitted or null, so rolling upgrades cannot strand
    /// legacy attachments.
    #[crate::sqlx_test]
    async fn legacy_attachments_with_omitted_or_null_ids_remain_writable(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.expect("begin legacy attachment setup");
        let instance_id = seed_instance(txn.as_mut(), 0x4a, None).await;
        let legacy = serde_json::json!({
            "service_configs": [
                {
                    "service_id": ExtensionServiceId::new(),
                    "version": ConfigVersion::initial(),
                    "dpu_target": null,
                    "removed": null
                },
                {
                    "id": null,
                    "service_id": ExtensionServiceId::new(),
                    "version": ConfigVersion::initial(),
                    "dpu_target": null,
                    "removed": null
                }
            ]
        });

        // Model outgoing writers that omitted an ID or serialized it as null.
        sqlx::query("UPDATE instances SET extension_services_config = $1 WHERE id = $2")
            .bind(Json(&legacy))
            .bind(instance_id)
            .execute(txn.as_mut())
            .await
            .expect("persist predecessor attachment JSON");
        txn.commit().await.expect("commit predecessor attachment");

        // Load the committed row again and preserve both forms as absent identities.
        let initial = find_by_id(&pool, instance_id)
            .await
            .expect("read older attachment")
            .expect("seeded instance exists");
        assert!(
            initial
                .config
                .extension_services
                .service_configs
                .iter()
                .all(|attachment| attachment.id.is_none())
        );
        let previous_version = initial.extension_services_config_version;

        // Lock and reload the predecessor row as a current writer would before
        // changing its attachment configuration.
        let mut txn = pool.begin().await.expect("begin current attachment update");
        find_by_id_for_update(txn.as_mut(), instance_id)
            .await
            .expect("lock older attachment")
            .expect("seeded instance exists");
        let locked = find_by_id(txn.as_mut(), instance_id)
            .await
            .expect("reload locked older attachment")
            .expect("seeded instance exists");
        let expected_config = locked.config.extension_services;
        let mut updated_config = expected_config.clone();

        // Mark both attachments for removal. This is the content change that
        // proves predecessor JSON remains writable, not merely readable.
        for attachment in &mut updated_config.service_configs {
            attachment.removed = Some(Utc::now());
        }
        assert_eq!(
            update_extension_services_config(
                txn.as_mut(),
                instance_id,
                previous_version,
                &expected_config,
                &updated_config,
                true,
            )
            .await
            .expect("update older attachment"),
            ConditionalWrite::Applied(())
        );
        txn.commit()
            .await
            .expect("commit current attachment update");

        // A current writer rewrites both forms to the canonical omitted representation.
        let stored: serde_json::Value =
            sqlx::query_scalar("SELECT extension_services_config FROM instances WHERE id = $1")
                .bind(instance_id)
                .fetch_one(&pool)
                .await
                .expect("read rewritten attachment JSON");
        let stored_configs = stored["service_configs"]
            .as_array()
            .expect("stored attachment list");
        assert_eq!(stored_configs.len(), 2);
        assert!(
            stored_configs
                .iter()
                .all(|attachment| attachment.get("id").is_none())
        );
        let persisted = find_by_id(&pool, instance_id)
            .await
            .expect("reread updated older attachment")
            .expect("seeded instance exists");

        // A fresh model read must contain the complete requested removal
        // update; accepting and normalizing the legacy JSON is not enough.
        assert_eq!(persisted.config.extension_services, updated_config);
        assert_eq!(
            persisted.extension_services_config_version.version_nr(),
            previous_version.version_nr() + 1
        );
    }

    /// Verifies the service-network writer atomically writes `extension_services_config`,
    /// `network_config`, and their configuration versions; accepts supported historical network
    /// JSON; and rejects mismatched or malformed configurations so the two writes cannot split.
    #[crate::sqlx_test]
    async fn service_network_writer_publishes_both_configs_or_rejects_stale_state(
        pool: sqlx::PgPool,
    ) {
        // Seed separate live instances for the writer contract and historical JSON comparisons.
        let mut setup = pool.begin().await.expect("begin service-network setup");
        let instance_id = seed_instance(setup.as_mut(), 0x4b, None).await;
        let compatibility_instance_id = seed_instance(setup.as_mut(), 0x4c, None).await;
        setup.commit().await.expect("commit service-network setup");

        // Confirm the baseline omits `service_interfaces` so its serde default compares equal.
        let service_interfaces_missing: bool = sqlx::query_scalar(
            "SELECT NOT network_config ? 'service_interfaces' FROM instances WHERE id=$1",
        )
        .bind(instance_id)
        .fetch_one(&pool)
        .await
        .expect("inspect legacy network JSON");
        assert!(service_interfaces_missing);
        let baseline = find_by_id(&pool, instance_id)
            .await
            .expect("read service-network baseline")
            .expect("seeded instance exists");

        // Model a historical explicit interface whose omitted optional fields deserialize to the
        // same current values, while `function_id` and `internal_uuid` remain present.
        let first_segment_id = NetworkSegmentId::new();
        let second_segment_id = NetworkSegmentId::new();
        let compatibility_network = InstanceNetworkConfig {
            interfaces: vec![
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Physical {},
                    network_details: None,
                    vpc_selection: None,
                    // Retaining this non-default value proves normalization does not erase known data.
                    network_segment_id: Some(first_segment_id),
                    ip_addrs: HashMap::default(),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::default(),
                    network_segment_gateways: HashMap::default(),
                    host_inband_mac_address: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::nil(),
                    vpc_id: None,
                },
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Virtual { id: 1 },
                    network_details: None,
                    vpc_selection: None,
                    // Distinct interface data ensures the JSON array order remains significant.
                    network_segment_id: Some(second_segment_id),
                    ip_addrs: HashMap::default(),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::default(),
                    network_segment_gateways: HashMap::default(),
                    host_inband_mac_address: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::new_v4(),
                    vpc_id: None,
                },
            ],
            auto_config: None,
            service_interfaces: Vec::new(),
        };
        let compatibility_cases = [
            // Legacy auto=false carries the same explicit-networking intent as auto_config=None.
            ("legacy auto false", Some(false), true),
            // Pre-auto JSON omits both representations and still means explicit networking.
            ("legacy auto omitted", None, true),
            // Legacy auto=true lacks `auto_config.vpc_id`, so it cannot reconstruct current `auto_config`.
            ("legacy auto true", Some(true), false),
        ];
        for (scenario, legacy_auto, should_apply) in compatibility_cases {
            // Remove only fields whose current serde defaults reproduce the compatibility model.
            let mut legacy_network =
                serde_json::to_value(&compatibility_network).expect("serialize current network");
            let legacy_network_object = legacy_network
                .as_object_mut()
                .expect("network config is a JSON object");
            legacy_network_object.remove("auto_config");
            legacy_network_object.remove("service_interfaces");
            legacy_network_object
                .extend(legacy_auto.map(|auto| ("auto".to_string(), serde_json::json!(auto))));
            let legacy_interfaces = legacy_network_object
                .get_mut("interfaces")
                .and_then(serde_json::Value::as_array_mut)
                .expect("legacy interfaces are a JSON array");
            for legacy_interface in legacy_interfaces.iter_mut() {
                let legacy_interface = legacy_interface
                    .as_object_mut()
                    .expect("legacy interface is a JSON object");
                for field in [
                    "network_details",
                    "ip_addrs",
                    "requested_ip_addr",
                    "ipv6",
                    "routing_profile",
                    "interface_prefixes",
                    "network_segment_gateways",
                    "host_inband_mac_address",
                    "device_locator",
                    "vpc_id",
                ] {
                    legacy_interface.remove(field);
                }
            }
            // Persist each raw historical representation without advancing either configuration version.
            sqlx::query("UPDATE instances SET network_config=$2::jsonb WHERE id=$1")
                .bind(compatibility_instance_id)
                .bind(Json(&legacy_network))
                .execute(&pool)
                .await
                .expect("seed historical network JSON");

            // Reload through the model to prove the historical omissions retain current semantics.
            let compatibility_baseline = find_by_id(&pool, compatibility_instance_id)
                .await
                .expect("read historical network JSON")
                .expect("compatibility instance exists");
            assert_eq!(compatibility_baseline.config.network, compatibility_network);

            // Compare the current model against raw historical JSON without changing either configuration version.
            let mut compatibility_writer = pool.begin().await.expect("begin compatibility writer");
            let outcome = update_service_network_config(
                compatibility_writer.as_mut(),
                compatibility_instance_id,
                compatibility_baseline.extension_services_config_version,
                &compatibility_baseline.config.extension_services,
                &compatibility_baseline.config.extension_services,
                compatibility_baseline.network_config_version,
                &compatibility_network,
                &compatibility_network,
                false,
            )
            .await
            .expect("compare historical network JSON");
            assert_eq!(
                matches!(outcome, ConditionalWrite::Applied(_)),
                should_apply,
                "{scenario}"
            );
            compatibility_writer
                .rollback()
                .await
                .expect("rollback compatibility writer");

            // Roll back any accepted write, then confirm the raw historical JSON is restored.
            let stored_network: serde_json::Value =
                sqlx::query_scalar("SELECT network_config FROM instances WHERE id=$1")
                    .bind(compatibility_instance_id)
                    .fetch_one(&pool)
                    .await
                    .expect("read historical network JSON after comparison");
            assert_eq!(stored_network, legacy_network, "{scenario}");
        }

        // Seed compatible historical JSON with non-default interface values, then change only the
        // expected value to prove the compatibility comparison still enforces known data.
        let mut nondefault_legacy_network =
            serde_json::to_value(&compatibility_network).expect("serialize compatibility network");
        let nondefault_legacy_object = nondefault_legacy_network
            .as_object_mut()
            .expect("compatibility network is a JSON object");
        nondefault_legacy_object.remove("auto_config");
        nondefault_legacy_object.remove("service_interfaces");
        nondefault_legacy_object.insert("auto".to_string(), serde_json::json!(false));
        sqlx::query("UPDATE instances SET network_config=$2::jsonb WHERE id=$1")
            .bind(compatibility_instance_id)
            .bind(Json(&nondefault_legacy_network))
            .execute(&pool)
            .await
            .expect("seed non-default historical network JSON");
        let compatibility_baseline = find_by_id(&pool, compatibility_instance_id)
            .await
            .expect("read non-default historical network JSON")
            .expect("compatibility instance exists");
        let mut changed_value_network = compatibility_network.clone();
        changed_value_network.interfaces[0].network_segment_id = Some(NetworkSegmentId::new());
        let mut reordered_network = compatibility_network.clone();
        reordered_network.interfaces.swap(0, 1);
        let significant_value_cases = [
            // A changed known field must still make the conditional write return `NotApplied`.
            ("changed non-default value", changed_value_network),
            // Reordering distinct interfaces must still make the conditional write return `NotApplied`.
            ("reordered interfaces", reordered_network),
        ];
        for (scenario, expected_network) in significant_value_cases {
            // Compare against the same raw row and configuration versions while varying one expected value.
            let mut mismatched_writer = pool
                .begin()
                .await
                .expect("begin historical mismatch writer");
            assert_eq!(
                update_service_network_config(
                    mismatched_writer.as_mut(),
                    compatibility_instance_id,
                    compatibility_baseline.extension_services_config_version,
                    &compatibility_baseline.config.extension_services,
                    &compatibility_baseline.config.extension_services,
                    compatibility_baseline.network_config_version,
                    &expected_network,
                    &expected_network,
                    false,
                )
                .await
                .expect("reject significant historical mismatch"),
                ConditionalWrite::NotApplied(InstanceServiceNetworkNotCurrent),
                "{scenario}"
            );
            mismatched_writer
                .rollback()
                .await
                .expect("rollback historical mismatch writer");
        }

        let invalid_shape_cases = [
            // A missing required interfaces array must never normalize into an empty current array.
            ("missing interfaces", serde_json::json!({"auto": false})),
            // A null interfaces value is invalid and must fail closed without raising a SQL error.
            (
                "null interfaces",
                serde_json::json!({"auto": false, "interfaces": null}),
            ),
            // A non-array interfaces value is invalid and must fail closed without invoking the SRF.
            (
                "scalar interfaces",
                serde_json::json!({"auto": false, "interfaces": 7}),
            ),
        ];
        for (scenario, invalid_network) in invalid_shape_cases {
            // Write malformed network JSON that `InstanceNetworkConfig` cannot deserialize.
            sqlx::query("UPDATE instances SET network_config=$2::jsonb WHERE id=$1")
                .bind(compatibility_instance_id)
                .bind(Json(&invalid_network))
                .execute(&pool)
                .await
                .expect("seed invalid historical network JSON");

            // The compatibility predicate must reject malformed JSON as a mismatched network configuration.
            let mut invalid_writer = pool
                .begin()
                .await
                .expect("begin invalid-shape compatibility writer");
            assert_eq!(
                update_service_network_config(
                    invalid_writer.as_mut(),
                    compatibility_instance_id,
                    compatibility_baseline.extension_services_config_version,
                    &compatibility_baseline.config.extension_services,
                    &compatibility_baseline.config.extension_services,
                    compatibility_baseline.network_config_version,
                    &InstanceNetworkConfig::default(),
                    &InstanceNetworkConfig::default(),
                    false,
                )
                .await
                .expect("reject invalid historical network JSON"),
                ConditionalWrite::NotApplied(InstanceServiceNetworkNotCurrent),
                "{scenario}"
            );
            invalid_writer
                .rollback()
                .await
                .expect("rollback invalid-shape compatibility writer");
        }

        // Build current automatic-networking intent so both compatible and contradictory historical
        // representations exercise the transition from auto_config to service_interfaces.
        let automatic_network = InstanceNetworkConfig {
            auto_config: Some(InstanceNetworkAutoConfig {
                vpc_id: VpcId::new(),
            }),
            ..InstanceNetworkConfig::default()
        };
        let mut conflicting_auto_network =
            serde_json::to_value(&automatic_network).expect("serialize automatic network");
        conflicting_auto_network
            .as_object_mut()
            .expect("automatic network is a JSON object")
            .remove("service_interfaces");

        // Persist the post-auto_config, pre-service_interfaces representation.
        sqlx::query("UPDATE instances SET network_config=$2::jsonb WHERE id=$1")
            .bind(compatibility_instance_id)
            .bind(Json(&conflicting_auto_network))
            .execute(&pool)
            .await
            .expect("seed historical automatic network JSON");
        let mut automatic_writer = pool
            .begin()
            .await
            .expect("begin historical automatic-network writer");

        // Accept the historical row because omitting an empty `service_interfaces` array changes no value.
        assert!(matches!(
            update_service_network_config(
                automatic_writer.as_mut(),
                compatibility_instance_id,
                compatibility_baseline.extension_services_config_version,
                &compatibility_baseline.config.extension_services,
                &compatibility_baseline.config.extension_services,
                compatibility_baseline.network_config_version,
                &automatic_network,
                &automatic_network,
                false,
            )
            .await
            .expect("accept historical automatic-network representation"),
            ConditionalWrite::Applied(_)
        ));
        automatic_writer
            .rollback()
            .await
            .expect("rollback historical automatic-network writer");

        // Adding legacy auto=false contradicts current automatic-networking intent and must reject.
        conflicting_auto_network
            .as_object_mut()
            .expect("automatic network is a JSON object")
            .insert("auto".to_string(), serde_json::json!(false));
        sqlx::query("UPDATE instances SET network_config=$2::jsonb WHERE id=$1")
            .bind(compatibility_instance_id)
            .bind(Json(&conflicting_auto_network))
            .execute(&pool)
            .await
            .expect("seed conflicting automatic network JSON");

        // Current automatic networking cannot compare equal to a row that also claims legacy false.
        let mut conflicting_auto_writer = pool
            .begin()
            .await
            .expect("begin conflicting automatic-network writer");
        assert_eq!(
            update_service_network_config(
                conflicting_auto_writer.as_mut(),
                compatibility_instance_id,
                compatibility_baseline.extension_services_config_version,
                &compatibility_baseline.config.extension_services,
                &compatibility_baseline.config.extension_services,
                compatibility_baseline.network_config_version,
                &automatic_network,
                &automatic_network,
                false,
            )
            .await
            .expect("reject conflicting automatic-network representations"),
            ConditionalWrite::NotApplied(InstanceServiceNetworkNotCurrent)
        );
        conflicting_auto_writer
            .rollback()
            .await
            .expect("rollback conflicting automatic-network writer");

        // Build one attachment and service interface with the same `attachment_id`; the remaining
        // service-interface fields make the stored record complete but do not drive this predicate.
        let attachment_id = uuid::Uuid::new_v4();
        let mut extension_services = baseline.config.extension_services.clone();
        extension_services
            .service_configs
            .push(InstanceExtensionServiceConfig {
                id: Some(attachment_id),
                service_id: ExtensionServiceId::new(),
                version: ConfigVersion::initial(),
                dpu_target: Some(DpuTarget::All),
                removed: None,
            });
        let dpu_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x5b; 32],
            MachineType::Dpu,
        )
        .try_into()
        .expect("DPU machine ID");
        let mut network = baseline.config.network.clone();
        network
            .service_interfaces
            .push(InstanceServiceInterfaceConfig {
                attachment_id,
                interface_ordinal: 0,
                dpu_id,
                slot_index: 0,
                vpc_id: VpcId::new(),
                vpc_prefix_id: VpcPrefixId::new(),
                network_segment_id: NetworkSegmentId::new(),
                network_prefix_id: NetworkPrefixId::new(),
                link_prefix: "192.0.2.0/31".parse().expect("valid service link"),
                mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
                internal_uuid: uuid::Uuid::new_v4(),
            });
        let second_dpu_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x5c; 32],
            MachineType::Dpu,
        )
        .try_into()
        .expect("DPU machine ID");
        network
            .service_interfaces
            .push(InstanceServiceInterfaceConfig {
                attachment_id,
                interface_ordinal: 0,
                dpu_id: second_dpu_id,
                slot_index: 0,
                vpc_id: VpcId::new(),
                vpc_prefix_id: VpcPrefixId::new(),
                network_segment_id: NetworkSegmentId::new(),
                network_prefix_id: NetworkPrefixId::new(),
                link_prefix: "198.51.100.0/31".parse().expect("valid service link"),
                mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
                internal_uuid: uuid::Uuid::new_v4(),
            });

        // Apply both configurations and explicitly roll back the transaction, proving neither
        // configuration nor configuration version can escape an aborted caller transaction.
        let mut rolled_back = pool
            .begin()
            .await
            .expect("begin rolled-back service-network write");
        assert!(matches!(
            update_service_network_config(
                rolled_back.as_mut(),
                instance_id,
                baseline.extension_services_config_version,
                &baseline.config.extension_services,
                &extension_services,
                baseline.network_config_version,
                &baseline.config.network,
                &network,
                true,
            )
            .await
            .expect("write service-network state before rollback"),
            ConditionalWrite::Applied(_)
        ));
        rolled_back
            .rollback()
            .await
            .expect("rollback service-network write");
        let after_rollback = find_by_id(&pool, instance_id)
            .await
            .expect("read state after service-network rollback")
            .expect("seeded instance remains");
        assert_eq!(
            after_rollback.config.extension_services,
            baseline.config.extension_services
        );
        assert_eq!(after_rollback.config.network, baseline.config.network);
        assert_eq!(
            after_rollback.extension_services_config_version,
            baseline.extension_services_config_version
        );
        assert_eq!(
            after_rollback.network_config_version,
            baseline.network_config_version
        );

        // Write the attachment and service interface through one conditional database statement.
        let mut writer = pool.begin().await.expect("begin service-network write");
        let applied = update_service_network_config(
            writer.as_mut(),
            instance_id,
            baseline.extension_services_config_version,
            &baseline.config.extension_services,
            &extension_services,
            baseline.network_config_version,
            &baseline.config.network,
            &network,
            true,
        )
        .await
        .expect("write service-network state");
        assert!(matches!(applied, ConditionalWrite::Applied(_)));
        writer.commit().await.expect("commit service-network write");

        // Reload the Instance to prove both configurations and their configuration versions became visible together.
        let persisted = find_by_id(&pool, instance_id)
            .await
            .expect("read published service-network state")
            .expect("seeded instance remains");
        assert_eq!(persisted.config.extension_services, extension_services);
        assert_eq!(persisted.config.network, network);
        assert_eq!(
            persisted.extension_services_config_version.version_nr(),
            baseline.extension_services_config_version.version_nr() + 1
        );
        assert_eq!(
            persisted.network_config_version.version_nr(),
            baseline.network_config_version.version_nr() + 1
        );

        let stale_generation_cases = [
            // A stale `extension_services_config_version` must fail when both configurations and
            // `network_config_version` are current.
            (
                "stale extension generation",
                baseline.extension_services_config_version,
                persisted.network_config_version,
            ),
            // A stale `network_config_version` must fail when both configurations and
            // `extension_services_config_version` are current.
            (
                "stale network generation",
                persisted.extension_services_config_version,
                baseline.network_config_version,
            ),
        ];
        for (scenario, extension_version, network_version) in stale_generation_cases {
            // Retry with current configurations and exactly one stale configuration version.
            let mut stale = pool
                .begin()
                .await
                .expect("begin stale service-network write");
            assert_eq!(
                update_service_network_config(
                    stale.as_mut(),
                    instance_id,
                    extension_version,
                    &persisted.config.extension_services,
                    &extension_services,
                    network_version,
                    &persisted.config.network,
                    &network,
                    true,
                )
                .await
                .expect("reject stale service-network generation"),
                ConditionalWrite::NotApplied(InstanceServiceNetworkNotCurrent),
                "{scenario}"
            );
            stale.rollback().await.expect("rollback stale writer");

            // A fresh Instance read proves the failed predicate prevented either configuration from changing.
            let after_stale = find_by_id(&pool, instance_id)
                .await
                .expect("read state after stale writer")
                .expect("seeded instance remains");
            assert_eq!(
                after_stale.config.extension_services, extension_services,
                "{scenario}"
            );
            assert_eq!(after_stale.config.network, network, "{scenario}");
            assert_eq!(
                after_stale.extension_services_config_version,
                persisted.extension_services_config_version,
                "{scenario}"
            );
            assert_eq!(
                after_stale.network_config_version, persisted.network_config_version,
                "{scenario}"
            );
        }

        let stale_document_cases = [
            // A mismatched expected `extension_services_config` must fail while both configuration
            // versions and `network_config` are current.
            (
                "stale extension JSON",
                &baseline.config.extension_services,
                &persisted.config.network,
            ),
            // A mismatched expected `network_config` must fail while both configuration versions and
            // `extension_services_config` are current.
            (
                "stale network JSON",
                &persisted.config.extension_services,
                &baseline.config.network,
            ),
        ];
        for (scenario, expected_extension_config, expected_network_config) in stale_document_cases {
            // Attempt to restore both baseline configurations without advancing either current configuration version.
            let mut stale = pool
                .begin()
                .await
                .expect("begin stale service-network document write");
            assert_eq!(
                update_service_network_config(
                    stale.as_mut(),
                    instance_id,
                    persisted.extension_services_config_version,
                    expected_extension_config,
                    &baseline.config.extension_services,
                    persisted.network_config_version,
                    expected_network_config,
                    &baseline.config.network,
                    false,
                )
                .await
                .expect("reject stale service-network document"),
                ConditionalWrite::NotApplied(InstanceServiceNetworkNotCurrent),
                "{scenario}"
            );
            stale
                .rollback()
                .await
                .expect("rollback stale document writer");

            // Current configurations and configuration versions remain paired after either predicate fails.
            let after_stale = find_by_id(&pool, instance_id)
                .await
                .expect("read state after stale document writer")
                .expect("seeded instance remains");
            assert_eq!(
                after_stale.config.extension_services, extension_services,
                "{scenario}"
            );
            assert_eq!(after_stale.config.network, network, "{scenario}");
            assert_eq!(
                after_stale.extension_services_config_version,
                persisted.extension_services_config_version,
                "{scenario}"
            );
            assert_eq!(
                after_stale.network_config_version, persisted.network_config_version,
                "{scenario}"
            );
        }
    }

    /// Verifies observed cleanup cannot be overwritten by a service-network writer that read the
    /// same public configuration versions because cleanup intentionally preserves those versions.
    #[crate::sqlx_test]
    async fn service_network_writer_rejects_same_generation_cleanup_race(pool: sqlx::PgPool) {
        // Persist a terminating attachment whose observed cleanup may remove it without advancing
        // `extension_services_config_version`.
        let mut setup = pool.begin().await.expect("begin cleanup-race setup");
        let instance_id = seed_instance(setup.as_mut(), 0x4e, None).await;
        let terminating_attachment = InstanceExtensionServiceConfig {
            id: Some(uuid::Uuid::new_v4()),
            service_id: ExtensionServiceId::new(),
            version: ConfigVersion::initial(),
            dpu_target: Some(DpuTarget::Primary),
            removed: Some(Utc::now()),
        };
        let terminating_config = InstanceExtensionServicesConfig {
            service_configs: vec![terminating_attachment],
        };
        sqlx::query("UPDATE instances SET extension_services_config=$2 WHERE id=$1")
            .bind(instance_id)
            .bind(Json(&terminating_config))
            .execute(setup.as_mut())
            .await
            .expect("persist terminating attachment");
        setup.commit().await.expect("commit cleanup-race setup");
        let baseline = find_by_id(&pool, instance_id)
            .await
            .expect("read cleanup-race baseline")
            .expect("seeded instance exists");

        // Hold the cleanup update open so the competing writer reads the committed predecessor and
        // then waits while attempting to write its replacement.
        let cleaned_config = InstanceExtensionServicesConfig::default();
        let mut cleanup = pool.begin().await.expect("begin observed cleanup");
        assert_eq!(
            update_extension_services_config(
                cleanup.as_mut(),
                instance_id,
                baseline.extension_services_config_version,
                &baseline.config.extension_services,
                &cleaned_config,
                false,
            )
            .await
            .expect("remove observed terminating attachment"),
            ConditionalWrite::Applied(())
        );
        let cleanup_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(cleanup.as_mut())
            .await
            .expect("read cleanup backend PID");
        let mut replacement_config = baseline.config.extension_services.clone();
        replacement_config
            .service_configs
            .push(InstanceExtensionServiceConfig {
                id: Some(uuid::Uuid::new_v4()),
                service_id: ExtensionServiceId::new(),
                version: ConfigVersion::initial(),
                dpu_target: Some(DpuTarget::Primary),
                removed: None,
            });

        // Commit cleanup only after the writer reaches its final conditional update.
        let competing_writer = async {
            let mut writer = pool.begin().await.expect("begin competing writer");
            let outcome = update_service_network_config(
                writer.as_mut(),
                instance_id,
                baseline.extension_services_config_version,
                &baseline.config.extension_services,
                &replacement_config,
                baseline.network_config_version,
                &baseline.config.network,
                &baseline.config.network,
                true,
            )
            .await
            .expect("complete competing service-network writer");
            writer.rollback().await.expect("rollback competing writer");
            outcome
        };
        let release_cleanup = async {
            wait_for_blocked_query(&pool, cleanup_pid, "SET extension_services_config_version")
                .await;
            cleanup.commit().await.expect("commit observed cleanup");
        };
        let (outcome, ()) = tokio::join!(competing_writer, release_cleanup);

        // The exact configuration values reject the stale writer while retaining cleanup and both
        // public configuration versions.
        assert_eq!(
            outcome,
            ConditionalWrite::NotApplied(InstanceServiceNetworkNotCurrent)
        );
        let persisted = find_by_id(&pool, instance_id)
            .await
            .expect("read cleanup-race result")
            .expect("seeded instance remains");
        assert_eq!(persisted.config.extension_services, cleaned_config);
        assert_eq!(persisted.config.network, baseline.config.network);
        assert_eq!(
            persisted.extension_services_config_version,
            baseline.extension_services_config_version
        );
        assert_eq!(
            persisted.network_config_version,
            baseline.network_config_version
        );
    }

    /// Verifies reclamation rejects active attachments, mismatched service interfaces, and stale
    /// configuration versions so cleanup cannot remove current Instance data.
    #[crate::sqlx_test]
    async fn service_vpc_reclamation_rejects_active_mismatched_and_stale_state(pool: sqlx::PgPool) {
        // Seed one terminating attachment and one active attachment with independent resources.
        let common_pools = service_vpc_loopback_pools(&pool).await;
        let mut txn = pool
            .begin()
            .await
            .expect("begin service-VPC reclamation setup");
        let fixture = seed_service_vpc_reclamation_fixture(txn.as_mut(), &common_pools).await;

        let mut mismatched_endpoint = fixture.endpoint.clone();
        mismatched_endpoint.internal_uuid = uuid::Uuid::new_v4();
        let cases = [
            // An active attachment cannot be reclaimed even when its service interface matches.
            (
                "active attachment",
                fixture.survivor_attachment_id,
                fixture.baseline.extension_services_config_version,
                fixture.baseline.network_config_version,
                vec![fixture.survivor_endpoint.clone()],
            ),
            // A different `internal_uuid` cannot authorize removal of the stored service interface.
            (
                "mismatched service interface",
                fixture.attachment_id,
                fixture.baseline.extension_services_config_version,
                fixture.baseline.network_config_version,
                vec![mismatched_endpoint],
            ),
            // A stale `extension_services_config_version` must block reclamation while
            // `network_config_version` is current.
            (
                "stale extension-services configuration version",
                fixture.attachment_id,
                fixture
                    .baseline
                    .extension_services_config_version
                    .increment(),
                fixture.baseline.network_config_version,
                vec![fixture.endpoint.clone()],
            ),
            // A stale `network_config_version` must block reclamation while
            // `extension_services_config_version` is current.
            (
                "stale network configuration version",
                fixture.attachment_id,
                fixture.baseline.extension_services_config_version,
                fixture.baseline.network_config_version.increment(),
                vec![fixture.endpoint.clone()],
            ),
        ];
        for (scenario, attachment_id, extension_version, network_version, expected_endpoints) in
            cases
        {
            // Attempt reclamation with the case-specific attachment and expected service interfaces.
            assert_eq!(
                reclaim_service_vpc_attachment(
                    txn.as_mut(),
                    fixture.instance_id,
                    attachment_id,
                    extension_version,
                    network_version,
                    &expected_endpoints,
                    None,
                )
                .await
                .expect("reject service-VPC reclamation"),
                ConditionalWrite::NotApplied(ServiceVpcReclamationNotCurrent),
                "{scenario}"
            );

            // Both configurations, addresses, segments, and configuration versions remain unchanged.
            let after_rejection = find_by_id(txn.as_mut(), fixture.instance_id)
                .await
                .expect("read state after rejected reclamation")
                .expect("seeded instance remains");
            assert_eq!(
                after_rejection.config.extension_services, fixture.attachments,
                "{scenario}"
            );
            assert_eq!(
                after_rejection.config.network, fixture.network,
                "{scenario}"
            );
            assert_eq!(
                after_rejection.extension_services_config_version,
                fixture.baseline.extension_services_config_version,
                "{scenario}"
            );
            assert_eq!(
                after_rejection.network_config_version, fixture.baseline.network_config_version,
                "{scenario}"
            );
            assert_eq!(
                crate::instance_address::count_by_segment_id(txn.as_mut(), &fixture.segment_id)
                    .await
                    .expect("count target address after rejected reclamation"),
                1,
                "{scenario}"
            );
            assert_eq!(
                crate::instance_address::count_by_segment_id(
                    txn.as_mut(),
                    &fixture.survivor_segment_id,
                )
                .await
                .expect("count survivor address after rejected reclamation"),
                1,
                "{scenario}"
            );
            let target_segment_deleted: bool =
                sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_segments WHERE id=$1")
                    .bind(fixture.segment_id)
                    .fetch_one(txn.as_mut())
                    .await
                    .expect("read target segment after rejected reclamation");
            assert!(!target_segment_deleted, "{scenario}");
            let survivor_segment_deleted: bool =
                sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_segments WHERE id=$1")
                    .bind(fixture.survivor_segment_id)
                    .fetch_one(txn.as_mut())
                    .await
                    .expect("read survivor segment after rejected reclamation");
            assert!(!survivor_segment_deleted, "{scenario}");
        }

        txn.rollback()
            .await
            .expect("rollback rejected reclamation fixture");
    }

    /// Verifies reclamation keeps a VPC/DPU loopback while host networking retains the same
    /// membership because the host interface still depends on that loopback allocation.
    #[crate::sqlx_test]
    async fn service_vpc_reclamation_retains_loopback_with_host_membership(pool: sqlx::PgPool) {
        // Seed a terminating service interface whose loopback can also be retained by host networking.
        let common_pools = service_vpc_loopback_pools(&pool).await;
        let mut txn = pool
            .begin()
            .await
            .expect("begin retained-loopback reclamation setup");
        let fixture = seed_service_vpc_reclamation_fixture(txn.as_mut(), &common_pools).await;

        // Reclaim the service interface while host networking retains the same DPU/VPC membership.
        let retained_host_memberships = HashSet::from([(fixture.dpu_id, fixture.vpc_id)]);
        assert_eq!(
            reclaim_service_vpc_attachment(
                txn.as_mut(),
                fixture.instance_id,
                fixture.attachment_id,
                fixture.baseline.extension_services_config_version,
                fixture.baseline.network_config_version,
                std::slice::from_ref(&fixture.endpoint),
                Some(ServiceVpcLoopbackReleaseContext {
                    common_pools: &common_pools,
                    retained_host_memberships: &retained_host_memberships,
                }),
            )
            .await
            .expect("reclaim while host membership remains"),
            ConditionalWrite::Applied(())
        );

        // The VPC/DPU loopback row and its pool allocation remain available to host networking.
        assert!(
            crate::vpc_dpu_loopback::find(txn.as_mut(), &fixture.dpu_id, &fixture.vpc_id)
                .await
                .expect("read retained shared loopback")
                .is_some()
        );
        let retained_pool_entry =
            crate::resource_pool::find_value(txn.as_mut(), &fixture.target_loopback.to_string())
                .await
                .expect("read retained shared loopback allocation")
                .into_iter()
                .find(|entry| entry.pool_name == VPC_DPU_LOOPBACK)
                .expect("shared loopback remains in its resource pool");
        assert_eq!(
            retained_pool_entry.state.0,
            ResourcePoolEntryState::Allocated {
                owner: fixture.dpu_id.to_string(),
                owner_type: OwnerType::Machine.to_string(),
            }
        );

        txn.rollback()
            .await
            .expect("rollback retained-loopback reclamation fixture");
    }

    /// Verifies final reclamation removes only the matched terminating attachment, service
    /// interface, Instance address, network segment, retained slot, and VPC/DPU loopback while
    /// preserving the active attachment and both configuration versions.
    #[crate::sqlx_test]
    async fn service_vpc_reclamation_removes_target_and_preserves_survivor(pool: sqlx::PgPool) {
        // Seed one terminating target and one active survivor with independent persisted resources.
        let common_pools = service_vpc_loopback_pools(&pool).await;
        let mut txn = pool
            .begin()
            .await
            .expect("begin final service-VPC reclamation setup");
        let fixture = seed_service_vpc_reclamation_fixture(txn.as_mut(), &common_pools).await;
        assert_eq!(
            highest_retained_service_vpc_slot(txn.as_mut(), fixture.service_id)
                .await
                .expect("read target slot before reclamation"),
            Some(0)
        );
        assert_eq!(
            highest_retained_service_vpc_slot(txn.as_mut(), fixture.survivor_service_id)
                .await
                .expect("read survivor slot before reclamation"),
            Some(1)
        );

        // Reclaim without a retained host membership so the final target loopback can be released.
        let retained_host_memberships = HashSet::new();
        assert_eq!(
            reclaim_service_vpc_attachment(
                txn.as_mut(),
                fixture.instance_id,
                fixture.attachment_id,
                fixture.baseline.extension_services_config_version,
                fixture.baseline.network_config_version,
                std::slice::from_ref(&fixture.endpoint),
                Some(ServiceVpcLoopbackReleaseContext {
                    common_pools: &common_pools,
                    retained_host_memberships: &retained_host_memberships,
                }),
            )
            .await
            .expect("reclaim exact service-VPC attachment"),
            ConditionalWrite::Applied(())
        );

        // Reload every affected row and both configuration versions to prove exact cleanup.
        let reclaimed = find_by_id(txn.as_mut(), fixture.instance_id)
            .await
            .expect("read reclaimed instance")
            .expect("seeded instance remains");
        assert_eq!(
            reclaimed.config.extension_services.service_configs,
            vec![fixture.attachments.service_configs[1].clone()]
        );
        assert_eq!(
            reclaimed.config.network.service_interfaces,
            vec![fixture.survivor_endpoint.clone()]
        );
        assert_eq!(
            reclaimed.extension_services_config_version,
            fixture.baseline.extension_services_config_version
        );
        assert_eq!(
            reclaimed.network_config_version,
            fixture.baseline.network_config_version
        );
        assert_eq!(
            crate::instance_address::count_by_segment_id(txn.as_mut(), &fixture.segment_id)
                .await
                .expect("count reclaimed normalized address"),
            0
        );
        assert_eq!(
            highest_retained_service_vpc_slot(txn.as_mut(), fixture.service_id)
                .await
                .expect("read target slot after reclamation"),
            None
        );
        assert_eq!(
            highest_retained_service_vpc_slot(txn.as_mut(), fixture.survivor_service_id)
                .await
                .expect("read survivor slot after reclamation"),
            Some(1)
        );
        let segment_deleted: bool =
            sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_segments WHERE id=$1")
                .bind(fixture.segment_id)
                .fetch_one(txn.as_mut())
                .await
                .expect("read reclaimed segment state");
        assert!(segment_deleted);
        assert_eq!(
            crate::instance_address::count_by_segment_id(
                txn.as_mut(),
                &fixture.survivor_segment_id,
            )
            .await
            .expect("count surviving normalized address"),
            1
        );
        let survivor_segment_deleted: bool =
            sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_segments WHERE id=$1")
                .bind(fixture.survivor_segment_id)
                .fetch_one(txn.as_mut())
                .await
                .expect("read surviving segment state");
        assert!(!survivor_segment_deleted);
        assert!(
            crate::vpc_dpu_loopback::find(txn.as_mut(), &fixture.dpu_id, &fixture.vpc_id)
                .await
                .expect("read released service-only loopback")
                .is_none()
        );
        let released_pool_entry =
            crate::resource_pool::find_value(txn.as_mut(), &fixture.target_loopback.to_string())
                .await
                .expect("read released service-only loopback allocation")
                .into_iter()
                .find(|entry| entry.pool_name == VPC_DPU_LOOPBACK)
                .expect("released loopback remains in its resource pool");
        assert_eq!(released_pool_entry.state.0, ResourcePoolEntryState::Free);

        // Roll back after reloading the Instance configurations, addresses, segments, retained slot,
        // and loopback allocation.
        txn.rollback().await.expect("rollback reclamation fixture");
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
        let stored_instance = find_by_id(txn.as_mut(), instance_id)
            .await
            .unwrap()
            .expect("seeded instance exists");
        let mut network = stored_instance.config.network;
        network.interfaces.push(InstanceInterfaceConfig {
            function_id: InterfaceFunctionId::Physical {},
            network_details: None,
            vpc_selection: None,
            network_segment_id: Some(segment_id), // Drives the segment reference count.
            ip_addrs: HashMap::default(),
            requested_ip_addr: None,
            ipv6_interface_config: None,
            routing_profile: None,
            interface_prefixes: HashMap::default(),
            network_segment_gateways: HashMap::default(),
            host_inband_mac_address: None,
            device_locator: None,
            internal_uuid: uuid::Uuid::new_v4(),
            vpc_id: Some(vpc_id), // Drives the VPC reference count.
        });
        update_network_config(
            txn.as_mut(),
            instance_id,
            stored_instance.network_config_version,
            &network,
            false,
        )
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

    /// Verifies current endpoints owned by active and terminating attachments
    /// retain VPC and segment resources through soft deletion, while pending
    /// host-update service arrays cannot retain those resources after current
    /// endpoints disappear.
    #[crate::sqlx_test]
    async fn network_reference_counts_include_service_interfaces(pool: sqlx::PgPool) {
        // Give both current owners and the historical pending fixture distinct
        // resource IDs so a partial or broad union cannot satisfy the assertions.
        let mut txn = pool.begin().await.unwrap();
        let instance_id = seed_instance(&mut txn, 0x48, None).await;
        let active_segment_id = NetworkSegmentId::new();
        let active_vpc_id = VpcId::new();
        let terminating_segment_id = NetworkSegmentId::new();
        let terminating_vpc_id = VpcId::new();
        let pending_segment_id = NetworkSegmentId::new();
        let pending_vpc_id = VpcId::new();
        let active_dpu_id: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x48; 32],
            MachineType::Dpu,
        )
        .try_into()
        .unwrap();
        let terminating_dpu_id: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x49; 32],
            MachineType::Dpu,
        )
        .try_into()
        .unwrap();
        let active_attachment_id = uuid::Uuid::new_v4();
        let terminating_attachment_id = uuid::Uuid::new_v4();
        let attachments = InstanceExtensionServicesConfig {
            service_configs: vec![
                // Active attachment proves removal state is not required for ownership.
                InstanceExtensionServiceConfig {
                    id: Some(active_attachment_id),
                    service_id: ExtensionServiceId::new(),
                    version: ConfigVersion::initial(),
                    dpu_target: Some(DpuTarget::All),
                    removed: None,
                },
                // Terminating attachment proves cleanup retains ownership until endpoints vanish.
                InstanceExtensionServiceConfig {
                    id: Some(terminating_attachment_id),
                    service_id: ExtensionServiceId::new(),
                    version: ConfigVersion::initial(),
                    dpu_target: Some(DpuTarget::All),
                    removed: Some(Utc::now()),
                },
            ],
        };

        // Current state owns active and terminating memberships. Both pending
        // halves carry a third historical membership that is never authoritative.
        sqlx::query(
            "UPDATE instances SET extension_services_config = $2, \
                 network_config = jsonb_build_object( \
                 'interfaces', jsonb_build_array(), \
                 'service_interfaces', jsonb_build_array( \
                     jsonb_build_object( \
                         'attachment_id', $3::text, 'network_segment_id', $4::text, \
                         'vpc_id', $5::text, 'dpu_id', $6::text), \
                     jsonb_build_object( \
                         'attachment_id', $7::text, 'network_segment_id', $8::text, \
                         'vpc_id', $9::text, 'dpu_id', $10::text))), \
                 update_network_config_request = jsonb_build_object( \
                     'old_config', jsonb_build_object( \
                         'interfaces', jsonb_build_array(), \
                         'service_interfaces', jsonb_build_array(jsonb_build_object( \
                             'network_segment_id', $11::text, 'vpc_id', $12::text, \
                             'dpu_id', $6::text))), \
                     'new_config', jsonb_build_object( \
                         'interfaces', jsonb_build_array(), \
                         'service_interfaces', jsonb_build_array(jsonb_build_object( \
                             'network_segment_id', $11::text, 'vpc_id', $12::text, \
                             'dpu_id', $6::text))) \
                 ) \
             WHERE id = $1",
        )
        .bind(instance_id)
        .bind(Json(&attachments))
        .bind(active_attachment_id)
        .bind(active_segment_id)
        .bind(active_vpc_id)
        .bind(active_dpu_id)
        .bind(terminating_attachment_id)
        .bind(terminating_segment_id)
        .bind(terminating_vpc_id)
        .bind(terminating_dpu_id)
        .bind(pending_segment_id)
        .bind(pending_vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        // `find_service_vpc_memberships`, `count_network_segment_references`, and
        // `count_vpc_references` include both attachment lifecycles while
        // excluding historical pending endpoint arrays.
        assert_eq!(
            find_service_vpc_memberships(txn.as_mut(), instance_id)
                .await
                .unwrap()
                .into_iter()
                .collect::<HashSet<_>>(),
            HashSet::from([
                (active_dpu_id, active_vpc_id),
                (terminating_dpu_id, terminating_vpc_id),
            ]),
        );
        for (segment_id, vpc_id) in [
            // An active endpoint retains both resources without any removal marker.
            (active_segment_id, active_vpc_id),
            // A terminating endpoint retains both resources until endpoint cleanup.
            (terminating_segment_id, terminating_vpc_id),
        ] {
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
        }
        assert_eq!(
            count_network_segment_references(txn.as_mut(), &pending_segment_id)
                .await
                .unwrap(),
            0,
        );
        assert_eq!(
            count_vpc_references(txn.as_mut(), &pending_vpc_id)
                .await
                .unwrap(),
            0,
        );

        // Soft deletion retains both active and terminating endpoint owners.
        mark_as_deleted(instance_id, txn.as_mut()).await.unwrap();
        assert_eq!(
            find_service_vpc_memberships(txn.as_mut(), instance_id)
                .await
                .unwrap()
                .into_iter()
                .collect::<HashSet<_>>(),
            HashSet::from([
                (active_dpu_id, active_vpc_id),
                (terminating_dpu_id, terminating_vpc_id),
            ]),
        );
        for (segment_id, vpc_id) in [
            // Soft deletion does not release an active endpoint's ownership.
            (active_segment_id, active_vpc_id),
            // Soft deletion also preserves a terminating endpoint's cleanup ownership.
            (terminating_segment_id, terminating_vpc_id),
        ] {
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
        }

        // Clearing current endpoints releases every service reference even
        // though both pending configurations still contain historical arrays.
        sqlx::query(
            "UPDATE instances SET network_config = \
                 jsonb_set(network_config, '{service_interfaces}', '[]'::jsonb, true) \
             WHERE id = $1",
        )
        .bind(instance_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        assert!(
            find_service_vpc_memberships(txn.as_mut(), instance_id)
                .await
                .unwrap()
                .is_empty()
        );
        for (segment_id, vpc_id) in [
            // Clearing current endpoints releases the former active owner.
            (active_segment_id, active_vpc_id),
            // Clearing current endpoints releases the former terminating owner.
            (terminating_segment_id, terminating_vpc_id),
            // Historical pending arrays never establish ownership independently.
            (pending_segment_id, pending_vpc_id),
        ] {
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
        }
    }

    /// Verifies current service endpoints and later normalized address rows each
    /// block VPC deletion, segment marking, and final segment deletion to prevent
    /// orphaned service addresses during staged cleanup.
    #[crate::sqlx_test]
    async fn service_endpoint_and_normalized_address_guard_network_deletion(pool: sqlx::PgPool) {
        // Build the late-cleanup state in which the service endpoint is the
        // target VPC and segment's only remaining owner.
        let mut txn = pool.begin().await.unwrap();
        let instance_id = seed_instance(txn.as_mut(), 0x4c, None).await;
        let segment_id = NetworkSegmentId::new();
        let vpc_id = VpcId::new();
        let attachment_id = uuid::Uuid::new_v4();
        let dpu_id: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x4c; 32],
            MachineType::Dpu,
        )
        .try_into()
        .unwrap();
        sqlx::query(
            "INSERT INTO vpcs (id, name, organization_id, version) \
             VALUES ($1, 'service-boundary-vpc', 'service-boundary', 'V1-T0')",
        )
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id) \
             VALUES ($1, 'service-boundary-segment', 'V1-T0', $2)",
        )
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        // The attachment and resource IDs drive ownership. The remaining
        // endpoint fields provide a valid fixed-slot record for snapshot loading.
        let endpoint = InstanceServiceInterfaceConfig {
            attachment_id, // Connects the endpoint to its terminating attachment.
            interface_ordinal: 0,
            dpu_id,
            slot_index: 0,
            vpc_id, // Drives the VPC deletion guard.
            vpc_prefix_id: VpcPrefixId::new(),
            network_segment_id: segment_id, // Drives both segment deletion guards.
            network_prefix_id: NetworkPrefixId::new(),
            link_prefix: "192.0.2.0/31".parse().unwrap(),
            mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
            internal_uuid: uuid::Uuid::new_v4(),
        };
        let attachments = InstanceExtensionServicesConfig {
            service_configs: vec![InstanceExtensionServiceConfig {
                id: Some(attachment_id),
                service_id: ExtensionServiceId::new(),
                version: ConfigVersion::initial(),
                dpu_target: Some(DpuTarget::All),
                removed: Some(Utc::now()),
            }],
        };
        let mut network = InstanceNetworkConfig::default();
        network.service_interfaces.push(endpoint.clone());
        sqlx::query(
            "UPDATE instances \
             SET extension_services_config=$2, network_config=$3 \
             WHERE id=$1",
        )
        .bind(instance_id)
        .bind(Json(&attachments))
        .bind(Json(&network))
        .execute(txn.as_mut())
        .await
        .unwrap();

        // Reload config and count normalized occupancy to prove no host
        // interface or address masks the persisted service-only ownership.
        let persisted = find_by_id(txn.as_mut(), instance_id)
            .await
            .unwrap()
            .expect("service endpoint owner must remain persisted");
        assert!(persisted.config.network.interfaces.is_empty());
        assert_eq!(persisted.config.network.service_interfaces, vec![endpoint],);
        assert_eq!(
            crate::instance_address::count_by_segment_id(txn.as_mut(), &segment_id)
                .await
                .unwrap(),
            0,
        );
        let segment = crate::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(crate::network_segment::IdColumn, &segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap()
        .pop()
        .expect("target segment must remain persisted");

        // Exercise VPC deletion, segment marking, and final segment deletion
        // while the service endpoint is the only persisted owner.
        let vpc_error = crate::vpc::try_delete(txn.as_mut(), vpc_id)
            .await
            .expect_err("service endpoint must retain its VPC");
        assert!(matches!(
            vpc_error,
            DatabaseError::FailedPrecondition(message)
                if message.contains("1 instance network configurations still reference it")
        ));
        let mark_error = crate::network_segment::mark_as_deleted(&segment, txn.as_mut())
            .await
            .expect_err("service endpoint must prevent segment marking");
        assert!(matches!(
            mark_error,
            DatabaseError::NetworkSegmentDelete(message)
                if message.contains("referencing instance count: 1")
        ));
        let final_delete_error = crate::network_segment::final_delete(segment_id, &mut txn)
            .await
            .expect_err("service endpoint must prevent physical segment deletion");
        assert!(matches!(
            final_delete_error,
            DatabaseError::NetworkSegmentDelete(message)
                if message.contains("allocations still reference it")
        ));

        // Model endpoint cleanup occurring before its normalized allocation is
        // released, which must transfer ownership to the normalized fallback.
        let service_address: IpAddr = "192.0.2.1".parse().unwrap();
        sqlx::query(
            "INSERT INTO instance_addresses \
                (instance_id, address, prefix, segment_id, vpc_id) \
             VALUES ($1, $2, '192.0.2.0/31'::cidr, $3, $4)",
        )
        .bind(instance_id)
        .bind(service_address)
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE instances SET network_config = \
                 jsonb_set(network_config, '{service_interfaces}', '[]'::jsonb, true) \
             WHERE id=$1",
        )
        .bind(instance_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        let persisted = find_by_id(txn.as_mut(), instance_id)
            .await
            .unwrap()
            .expect("normalized address owner must remain persisted");
        assert!(persisted.config.network.service_interfaces.is_empty());

        // Recheck VPC deletion, segment marking, and final segment deletion so
        // the normalized allocation retains both resources after endpoint cleanup.
        let vpc_error = crate::vpc::try_delete(txn.as_mut(), vpc_id)
            .await
            .expect_err("normalized address must retain its VPC");
        assert!(matches!(
            vpc_error,
            DatabaseError::FailedPrecondition(message)
                if message.contains("1 instance addresses still reference it")
        ));
        let mark_error = crate::network_segment::mark_as_deleted(&segment, txn.as_mut())
            .await
            .expect_err("normalized address must prevent segment marking");
        assert!(matches!(
            mark_error,
            DatabaseError::NetworkSegmentDelete(message)
                if message.contains("addresses on the segment are allocated to instances")
        ));
        let final_delete_error = crate::network_segment::final_delete(segment_id, &mut txn)
            .await
            .expect_err("normalized address must prevent physical segment deletion");
        assert!(matches!(
            final_delete_error,
            DatabaseError::NetworkSegmentDelete(message)
                if message.contains("allocations still reference it")
        ));

        // Releasing the final normalized owner makes physical deletion safe.
        crate::instance_address::delete_addresses_for_instance(
            txn.as_mut(),
            instance_id,
            &[(segment_id, service_address)],
        )
        .await
        .unwrap();
        assert_eq!(
            crate::network_segment::final_delete(segment_id, &mut txn)
                .await
                .unwrap(),
            segment_id,
        );

        // Roll back the complete fixture so this proof leaves no persisted state.
        txn.rollback().await.unwrap();
    }

    /// Proves endpoint creation and whole-instance deletion serialize on the
    /// production address-allocation table lock and then the Instance row, so
    /// whichever transaction wins determines whether the endpoint/address pair
    /// or the physical deletion is retained.
    #[crate::sqlx_test]
    async fn delete_serializes_with_service_endpoint_writers(pool: sqlx::PgPool) {
        // Seed an endpoint-capable instance for the writer-first transaction.
        let mut setup = pool.begin().await.unwrap();
        let writer_wins_id = seed_instance(&mut setup, 0x4b, None).await;
        let writer_wins_segment = NetworkSegmentId::new();
        let writer_wins_vpc = VpcId::new();
        sqlx::query(
            "INSERT INTO vpcs (id, name, version) \
             VALUES ($1, 'writer-wins-vpc', 'V1-T0')",
        )
        .bind(writer_wins_vpc)
        .execute(setup.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id) \
             VALUES ($1, 'writer-wins-segment', 'V1-T0', $2)",
        )
        .bind(writer_wins_segment)
        .bind(writer_wins_vpc)
        .execute(setup.as_mut())
        .await
        .unwrap();
        let writer_wins_attachment = uuid::Uuid::new_v4();
        let writer_wins_services = InstanceExtensionServicesConfig {
            service_configs: vec![InstanceExtensionServiceConfig {
                id: Some(writer_wins_attachment),
                service_id: ExtensionServiceId::new(),
                version: ConfigVersion::initial(),
                dpu_target: Some(DpuTarget::All),
                removed: None,
            }],
        };
        sqlx::query("UPDATE instances SET extension_services_config = $1 WHERE id = $2")
            .bind(Json(&writer_wins_services))
            .bind(writer_wins_id)
            .execute(setup.as_mut())
            .await
            .unwrap();
        setup.commit().await.unwrap();

        // Commit an endpoint and its normalized address while deletion waits
        // on the allocation-table lock held by the same writer transaction.
        let baseline = find_by_id(&pool, writer_wins_id).await.unwrap().unwrap();
        let writer_wins_dpu: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x4b; 32],
            MachineType::Dpu,
        )
        .try_into()
        .unwrap();

        // Membership and attachment identity drive the deletion guard. The
        // remaining fields make the row a valid fixed-slot endpoint.
        let writer_wins_endpoint = InstanceServiceInterfaceConfig {
            attachment_id: writer_wins_attachment, // Connects this endpoint to its live owner.
            interface_ordinal: 0,
            dpu_id: writer_wins_dpu, // Selects the retained DPU/VPC membership.
            slot_index: 0,
            vpc_id: writer_wins_vpc, // Selects the retained DPU/VPC membership.
            vpc_prefix_id: VpcPrefixId::new(),
            network_segment_id: writer_wins_segment, // Joins the normalized allocation below.
            network_prefix_id: NetworkPrefixId::new(),
            link_prefix: "192.0.2.0/31".parse().unwrap(),
            mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 0x4b]),
            internal_uuid: uuid::Uuid::new_v4(),
        };
        let mut writer_network = baseline.config.network;
        writer_network
            .service_interfaces
            .push(writer_wins_endpoint.clone());
        let mut writer = pool.begin().await.unwrap();
        instance_address::lock_table_for_allocation(writer.as_mut())
            .await
            .unwrap();
        update_network_config(
            writer.as_mut(),
            writer_wins_id,
            baseline.network_config_version,
            &writer_network,
            false,
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO instance_addresses \
                (instance_id, address, prefix, segment_id, vpc_id) \
             VALUES ($1, '192.0.2.1'::inet, '192.0.2.0/31'::cidr, $2, $3)",
        )
        .bind(writer_wins_id)
        .bind(writer_wins_segment)
        .bind(writer_wins_vpc)
        .execute(writer.as_mut())
        .await
        .unwrap();
        let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(writer.as_mut())
            .await
            .unwrap();
        let blocked_delete = async {
            let mut txn = pool.begin().await.unwrap();
            let error = delete(writer_wins_id, txn.as_mut())
                .await
                .expect_err("committed endpoint must defeat deletion");
            txn.rollback().await.unwrap();
            error
        };
        let release_writer = async {
            wait_for_blocked_query(&pool, writer_pid, "FROM instance_addresses").await;
            writer.commit().await.unwrap();
        };
        let (error, ()) = tokio::join!(blocked_delete, release_writer);
        assert!(matches!(error, DatabaseError::FailedPrecondition(_)));

        // Fresh reads prove the winning writer retained both durable owners.
        let retained = find_by_id(&pool, writer_wins_id)
            .await
            .unwrap()
            .expect("writer-winning instance remains");
        assert_eq!(
            retained.config.network.service_interfaces,
            vec![writer_wins_endpoint]
        );
        let mut readback = pool.begin().await.unwrap();
        let retained_addresses = instance_address::find_all_by_instance_id_and_segment_id(
            readback.as_mut(),
            &writer_wins_id,
            &writer_wins_segment,
        )
        .await
        .unwrap();
        assert_eq!(retained_addresses.len(), 1);
        assert_eq!(
            (
                retained_addresses[0].address,
                retained_addresses[0].prefix,
                retained_addresses[0].segment_id,
                retained_addresses[0].vpc_id,
            ),
            (
                "192.0.2.1".parse().unwrap(),
                "192.0.2.0/31".parse().unwrap(),
                writer_wins_segment,
                writer_wins_vpc,
            )
        );
        readback.rollback().await.unwrap();

        // Delete a second instance inside an uncommitted transaction, then
        // prove a stale endpoint writer wakes only after deletion commits.
        let mut setup = pool.begin().await.unwrap();
        let delete_wins_id = seed_instance(&mut setup, 0x4c, None).await;
        setup.commit().await.unwrap();
        let delete_wins_baseline = find_by_id(&pool, delete_wins_id).await.unwrap().unwrap();
        let mut stale_network = delete_wins_baseline.config.network;
        // Adding a service interface is the stale mutation under test. Its
        // generated identities make it a distinct valid endpoint, while the
        // remaining fields provide only the complete stored representation.
        stale_network
            .service_interfaces
            .push(InstanceServiceInterfaceConfig {
                attachment_id: uuid::Uuid::new_v4(), // Distinguishes this attempted endpoint write.
                interface_ordinal: 0,
                dpu_id: writer_wins_dpu,
                slot_index: 0,
                vpc_id: VpcId::new(),
                vpc_prefix_id: VpcPrefixId::new(),
                network_segment_id: NetworkSegmentId::new(),
                network_prefix_id: NetworkPrefixId::new(),
                link_prefix: "198.51.100.0/31".parse().unwrap(),
                mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 0x4c]),
                internal_uuid: uuid::Uuid::new_v4(),
            });
        let mut deletion = pool.begin().await.unwrap();
        delete(delete_wins_id, deletion.as_mut()).await.unwrap();
        let deletion_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(deletion.as_mut())
            .await
            .unwrap();
        let blocked_writer = async {
            let mut txn = pool.begin().await.unwrap();
            instance_address::lock_table_for_allocation(txn.as_mut())
                .await
                .unwrap();
            let error = update_network_config(
                txn.as_mut(),
                delete_wins_id,
                delete_wins_baseline.network_config_version,
                &stale_network,
                false,
            )
            .await
            .expect_err("writer must not resurrect a deleted instance");
            txn.rollback().await.unwrap();
            error
        };
        let release_deletion = async {
            wait_for_blocked_query(&pool, deletion_pid, "LOCK TABLE instance_addresses").await;
            deletion.commit().await.unwrap();
        };
        let (error, ()) = tokio::join!(blocked_writer, release_deletion);
        assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
        assert!(find_by_id(&pool, delete_wins_id).await.unwrap().is_none());
    }

    /// Whole-instance deletion retains the instance and its normalized
    /// allocations while a service endpoint exists, because only observed
    /// service cleanup may authorize their final removal.
    #[crate::sqlx_test]
    async fn delete_retains_service_owned_resources_until_endpoint_removal(pool: sqlx::PgPool) {
        // Build the guarded instance graph plus an unrelated owner whose address
        // must survive the endpoint-rejected deletion and later successful deletion.
        let mut txn = pool.begin().await.unwrap();
        let instance_id = seed_instance(&mut txn, 0x4a, None).await;
        let segment_id = NetworkSegmentId::new();
        let vpc_id = VpcId::new();
        let unrelated_instance_id = seed_instance(&mut txn, 0x4b, None).await;
        let unrelated_segment_id = NetworkSegmentId::new();
        let unrelated_vpc_id = VpcId::new();
        let dpu_id: DpuMachineId = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x4a; 32],
            MachineType::Dpu,
        )
        .try_into()
        .unwrap();

        // Seed the normalized allocation and its current endpoint owner.
        sqlx::query(
            "INSERT INTO vpcs (id, name, version) \
             VALUES ($1, 'service-delete-guard-vpc', 'V1-T0')",
        )
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO vpcs (id, name, version) \
             VALUES ($1, 'unrelated-delete-guard-vpc', 'V1-T0')",
        )
        .bind(unrelated_vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id) \
             VALUES ($1, 'service-delete-guard-segment', 'V1-T0', $2)",
        )
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id) \
             VALUES ($1, 'unrelated-delete-guard-segment', 'V1-T0', $2)",
        )
        .bind(unrelated_segment_id)
        .bind(unrelated_vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        let stored_instance = find_by_id(txn.as_mut(), instance_id)
            .await
            .unwrap()
            .expect("seeded instance exists");
        let mut network = stored_instance.config.network;
        network
            .service_interfaces
            .push(InstanceServiceInterfaceConfig {
                attachment_id: uuid::Uuid::new_v4(),
                interface_ordinal: 0,
                dpu_id, // Drives the retained DPU/VPC membership.
                slot_index: 0,
                vpc_id, // Drives the retained DPU/VPC membership.
                vpc_prefix_id: VpcPrefixId::new(),
                network_segment_id: segment_id, // Joins the normalized allocation below.
                network_prefix_id: NetworkPrefixId::new(),
                link_prefix: "192.0.2.0/31".parse().unwrap(),
                mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 0x4a]),
                internal_uuid: uuid::Uuid::new_v4(),
            });
        update_network_config(
            txn.as_mut(),
            instance_id,
            stored_instance.network_config_version,
            &network,
            false,
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO instance_addresses \
                (instance_id, address, prefix, segment_id, vpc_id) \
             VALUES ($1, '198.51.100.1'::inet, '198.51.100.0/31'::cidr, $2, $3)",
        )
        .bind(unrelated_instance_id)
        .bind(unrelated_segment_id)
        .bind(unrelated_vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO instance_addresses \
                (instance_id, address, prefix, segment_id, vpc_id) \
             VALUES ($1, '192.0.2.1'::inet, '192.0.2.0/31'::cidr, $2, $3)",
        )
        .bind(instance_id)
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.as_mut())
        .await
        .unwrap();

        // The failed deletion must leave both durable owners unchanged.
        let error = delete(instance_id, txn.as_mut())
            .await
            .expect_err("service endpoint must block whole-instance deletion");
        assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
        let instance_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM instances WHERE id=$1)")
                .bind(instance_id)
                .fetch_one(txn.as_mut())
                .await
                .unwrap();
        assert!(instance_exists);
        let retained_addresses = crate::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance_id,
            &segment_id,
        )
        .await
        .unwrap();
        assert_eq!(
            retained_addresses
                .iter()
                .map(|address| (
                    address.instance_id,
                    address.address,
                    address.prefix,
                    address.segment_id,
                    address.vpc_id,
                ))
                .collect::<Vec<_>>(),
            vec![(
                instance_id,
                "192.0.2.1".parse().unwrap(),
                "192.0.2.0/31".parse().unwrap(),
                segment_id,
                vpc_id,
            )]
        );
        let unrelated_addresses = crate::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &unrelated_instance_id,
            &unrelated_segment_id,
        )
        .await
        .unwrap();
        assert_eq!(
            unrelated_addresses
                .iter()
                .map(|address| (
                    address.instance_id,
                    address.address,
                    address.prefix,
                    address.segment_id,
                    address.vpc_id,
                ))
                .collect::<Vec<_>>(),
            vec![(
                unrelated_instance_id,
                "198.51.100.1".parse().unwrap(),
                "198.51.100.0/31".parse().unwrap(),
                unrelated_segment_id,
                unrelated_vpc_id,
            )]
        );

        // Allow `db::instance::delete` to remove the instance and its remaining
        // normalized allocation once the endpoint is absent.
        sqlx::query(
            "UPDATE instances SET network_config = \
                 jsonb_set(network_config, '{service_interfaces}', '[]'::jsonb, true) \
             WHERE id = $1",
        )
        .bind(instance_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
        delete(instance_id, txn.as_mut()).await.unwrap();
        let instance_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM instances WHERE id=$1)")
                .bind(instance_id)
                .fetch_one(txn.as_mut())
                .await
                .unwrap();
        assert!(!instance_exists);
        assert!(
            crate::instance_address::find_all_by_instance_id_and_segment_id(
                txn.as_mut(),
                &instance_id,
                &segment_id,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert!(
            find_by_id(txn.as_mut(), unrelated_instance_id)
                .await
                .unwrap()
                .is_some()
        );
        let unrelated_addresses = crate::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &unrelated_instance_id,
            &unrelated_segment_id,
        )
        .await
        .unwrap();
        assert_eq!(
            unrelated_addresses
                .iter()
                .map(|address| (
                    address.instance_id,
                    address.address,
                    address.prefix,
                    address.segment_id,
                    address.vpc_id,
                ))
                .collect::<Vec<_>>(),
            vec![(
                unrelated_instance_id,
                "198.51.100.1".parse().unwrap(),
                "198.51.100.0/31".parse().unwrap(),
                unrelated_segment_id,
                unrelated_vpc_id,
            )]
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

    /// Verifies VPC searches include hidden service-interface selections,
    /// because operators must find every instance that keeps a VPC in use even
    /// when no tenant interface or address references it.
    #[crate::sqlx_test]
    async fn vpc_search_includes_service_interfaces(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let fixture_machine_seed = 0x49; // Only makes the fixture machine ID deterministic.
        let instance_id = seed_instance(&mut txn, fixture_machine_seed, None).await;
        let selected_vpc = VpcId::new();

        // The hidden service-interface record is the only reference to this VPC.
        sqlx::query(
            "UPDATE instances SET network_config = jsonb_build_object( \
                 'interfaces', jsonb_build_array(), \
                 'service_interfaces', jsonb_build_array(jsonb_build_object( \
                     'vpc_id', $2::text))) \
             WHERE id = $1",
        )
        .bind(instance_id)
        .bind(selected_vpc)
        .execute(txn.as_mut())
        .await
        .unwrap();

        // Both search entry points must find the instance through that hidden reference.
        let filter = model::instance::InstanceSearchFilter {
            vpc_id: Some(selected_vpc.to_string()),
            ..Default::default()
        };
        assert_eq!(
            find_ids(txn.as_mut(), filter.clone()).await.unwrap(),
            vec![instance_id]
        );
        assert_eq!(count_ids(txn.as_mut(), filter).await.unwrap(), 1);

        // An unrelated VPC must not match merely because a service interface exists.
        let unrelated_filter = model::instance::InstanceSearchFilter {
            vpc_id: Some(VpcId::new().to_string()),
            ..Default::default()
        };
        assert!(
            find_ids(txn.as_mut(), unrelated_filter.clone())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(count_ids(txn.as_mut(), unrelated_filter).await.unwrap(), 0);
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
