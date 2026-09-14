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

use std::collections::HashMap;

use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::{ConfigVersion, ConfigVersionChange};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::extension_service::{
    ExtensionService, ExtensionServiceLifecycleState, ExtensionServiceObservability,
    ExtensionServiceSnapshot, ExtensionServiceType, ExtensionServiceVersionInfo,
};
use model::tenant::TenantOrganizationId;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult};

/// Creates a new extension service and creates its initial extension service version.
/// It enforces a unique `(tenant_organization_id, name)` combination.
///
/// # Parameters
/// * `txn`                    - A reference to an active DB transaction
/// * `service_type`           - The type of the extension service
/// * `service_name`           - The name of the extension service
/// * `description`            - The description of the extension service
/// * `data`                   - Data of the initial version of the extension service
/// * `observability`          - Observability config for the extension service
/// * `has_credential`         - Whether the initial extension service version has a credential
///   stored in the vault
#[allow(clippy::too_many_arguments)]
pub async fn create(
    txn: &mut PgConnection,
    version: ConfigVersion,
    service_id: &ExtensionServiceId,
    service_type: &ExtensionServiceType,
    service_name: &str,
    tenant_organization_id: &TenantOrganizationId,
    description: Option<&str>,
    data: &str,
    observability: Option<ExtensionServiceObservability>,
    has_credential: bool,
) -> Result<(ExtensionService, ExtensionServiceVersionInfo), DatabaseError> {
    let initial_version_ctr = 1;
    let initial_controller_state = match service_type {
        ExtensionServiceType::KubernetesPod => ExtensionServiceLifecycleState::Ready,
        ExtensionServiceType::DpfHelmChart => ExtensionServiceLifecycleState::Creating,
    };
    // This version belongs solely to the asynchronous controller.  It must
    // not be derived from the API-visible service version counter.
    let initial_controller_state_version = ConfigVersion::initial();

    // First create the extension service record
    let service_query = "INSERT INTO extension_services
            (id, type, name, description, tenant_organization_id, version_ctr,
             controller_state, controller_state_version)
            VALUES ($1, $2::varchar, $3::varchar, $4::varchar, $5::varchar, $6::integer,
                    $7::jsonb, $8::varchar)
            RETURNING id, type, name, description, tenant_organization_id, version_ctr,
                      controller_state, controller_state_version, controller_state_outcome,
                      created, updated, deleted";

    let service = match sqlx::query_as::<_, ExtensionService>(service_query)
        .bind(service_id)
        .bind(service_type.to_string())
        .bind(service_name)
        .bind(description.unwrap_or(""))
        .bind(tenant_organization_id.to_string())
        .bind(initial_version_ctr)
        .bind(sqlx::types::Json(initial_controller_state))
        .bind(initial_controller_state_version)
        .fetch_one(&mut *txn)
        .await
    {
        Ok(service) => service,
        Err(sqlx::Error::Database(db_err))
            if db_err.is_unique_violation()
                && matches!(
                    db_err.constraint(),
                    Some("extension_services_tenant_lowername_unique")
                        | Some("extension_services_dpf_helm_chart_pending_name_unique")
                ) =>
        {
            return Err(DatabaseError::AlreadyFoundError {
                kind: "extension_service",
                id: format!("{}:{}", service_type, service_name),
            });
        }
        Err(e) => return Err(DatabaseError::query(service_query, e)),
    };

    // Insert the initial version using the service id
    let service_id = service.id;

    let version_query = "INSERT INTO extension_service_versions 
            (service_id, version, data, observability, has_credential)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING service_id, version, data, observability, has_credential, created, deleted";

    let version = sqlx::query_as::<_, ExtensionServiceVersionInfo>(version_query)
        .bind(service_id)
        .bind(version.to_string())
        .bind(data)
        .bind(observability.map(sqlx::types::Json))
        .bind(has_credential)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(version_query, e))?;

    // Kubernetes Pod services do not participate in this controller.  DPF
    // Helm services do, so record their initial Creating state in the same
    // transaction as the desired V1 data. A committed controller can never
    // observe a DPF service without its initial state-history record.
    if matches!(service_type, ExtensionServiceType::DpfHelmChart) {
        crate::state_history::persist(
            txn,
            crate::state_history::StateHistoryTableId::ExtensionService,
            &service_id,
            &initial_controller_state,
            initial_controller_state_version,
        )
        .await?;
    }

    Ok((service, version))
}

/// Updates an extension service by creating a new version.
/// - Always bumps `updated = now()` on the parent service and optionally updates metadata
///   (name/description)
/// - Inserts a new version with the next version number (1 + current latest version)
/// - Sets `has_credential` on the new version as provided
///
/// # Parameters
/// * `txn`                    - A reference to an active DB transaction
/// * `service_id`             - The id of the extension service to insert new version for
/// * `service_name`           - Optional new name of the extension service, must be unique within the tenant organization
/// * `description`            - Optional new description of the extension service
/// * `data`                   - Data of the new version of the extension service
/// * `observability`          - Observability config for the extension service
/// * `has_credential`         - Whether the new extension service version has a credential stored
///   in vault
#[allow(clippy::too_many_arguments)]
pub async fn update(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    service_name: Option<&str>,
    description: Option<&str>,
    data: &str,
    observability: Option<ExtensionServiceObservability>,
    has_credential: bool,
    config_version_change: ConfigVersionChange,
) -> Result<(ExtensionService, ExtensionServiceVersionInfo), DatabaseError> {
    // Update the "updated" timestamp of the extension service, and optionally update any provided
    // metadata (name, description)
    let mut builder =
        sqlx::QueryBuilder::new("UPDATE extension_services SET updated = CURRENT_TIMESTAMP");

    if let Some(name) = service_name {
        builder.push(", name = ");
        builder.push_bind(name);
    }
    if let Some(desc) = description {
        builder.push(", description = ");
        builder.push_bind(desc);
    }
    builder
        .push(", version_ctr = ")
        .push_bind(config_version_change.new.version_nr().cast_signed());
    builder.push(" WHERE id = ");
    builder.push_bind(service_id);
    builder
        .push(" AND version_ctr = ")
        .push_bind(config_version_change.current.version_nr().cast_signed());
    builder.push(" AND deleted IS NULL");
    builder.push(" RETURNING id, type, name, description, tenant_organization_id, version_ctr, controller_state, controller_state_version, controller_state_outcome, created, updated, deleted");

    let updated_service = match builder
        .build_query_as::<ExtensionService>()
        .fetch_one(&mut *txn)
        .await
    {
        Ok(service) => service,
        Err(sqlx::Error::RowNotFound) => {
            return Err(DatabaseError::NotFoundError {
                kind: "extension_service",
                id: service_id.to_string(),
            });
        }
        Err(sqlx::Error::Database(db_err))
            if db_err.is_unique_violation()
                && matches!(
                    db_err.constraint(),
                    Some("extension_services_tenant_lowername_unique")
                        | Some("extension_services_dpf_helm_chart_pending_name_unique")
                )
                && service_name.is_some() =>
        {
            return Err(DatabaseError::AlreadyFoundError {
                kind: "extension_service",
                id: format!("conflict on name {}", service_name.unwrap()),
            });
        }
        Err(e) => return Err(DatabaseError::query(builder.sql(), e)),
    };

    // Insert the new version with the next version number.
    // Since all updates will first take the extension service row for update, we do not need to worry
    // about concurrent update issue here.
    let version_query =
        "INSERT INTO extension_service_versions (service_id, version, data, observability, has_credential)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING service_id, version, data, observability, has_credential, created, deleted";

    let new_version = sqlx::query_as::<_, ExtensionServiceVersionInfo>(version_query)
        .bind(service_id)
        .bind(config_version_change.new)
        .bind(data)
        .bind(observability.map(sqlx::types::Json))
        .bind(has_credential)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(version_query, e))?;

    Ok((updated_service, new_version))
}

pub async fn update_metadata(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    service_name: Option<&str>,
    description: Option<&str>,
) -> Result<ExtensionService, DatabaseError> {
    // Update the "updated" timestamp of the extension service, and optionally update any provided
    // metadata (name, description)
    let mut builder =
        sqlx::QueryBuilder::new("UPDATE extension_services SET updated = CURRENT_TIMESTAMP");

    if let Some(name) = service_name {
        builder.push(", name = ");
        builder.push_bind(name);
    }
    if let Some(desc) = description {
        builder.push(", description = ");
        builder.push_bind(desc);
    }
    builder.push(" WHERE id = ");
    builder.push_bind(service_id);
    builder.push(" AND deleted IS NULL");
    builder.push(" RETURNING id, type, name, description, tenant_organization_id, version_ctr, controller_state, controller_state_version, controller_state_outcome, created, updated, deleted");

    let updated_service = match builder
        .build_query_as::<ExtensionService>()
        .fetch_one(&mut *txn)
        .await
    {
        Ok(service) => service,
        Err(sqlx::Error::RowNotFound) => {
            return Err(DatabaseError::NotFoundError {
                kind: "extension_service",
                id: service_id.to_string(),
            });
        }
        Err(sqlx::Error::Database(db_err))
            if db_err.is_unique_violation()
                && matches!(
                    db_err.constraint(),
                    Some("extension_services_tenant_lowername_unique")
                        | Some("extension_services_dpf_helm_chart_pending_name_unique")
                )
                && service_name.is_some() =>
        {
            return Err(DatabaseError::AlreadyFoundError {
                kind: "extension_service",
                id: format!("conflict on name {}", service_name.unwrap()),
            });
        }
        Err(e) => return Err(DatabaseError::query(builder.sql(), e)),
    };

    Ok(updated_service)
}

/// Atomically replaces the stable V1 desired definition of a Ready DPF Helm
/// chart service and requests asynchronous reconciliation of that replacement.
///
/// A DPF Helm chart extension service has exactly one
/// version, V1, for its lifetime. Unlike Kubernetes Pod updates, this replaces
/// V1 in place and never creates V2; all instance attachments continue to
/// reference V1 while the DPUService is patched in place by the controller.
#[allow(clippy::too_many_arguments)]
pub async fn update_dpf_helm_chart_in_place(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    service_name: Option<&str>,
    description: Option<&str>,
    normalized_data: &str,
    stable_version: ConfigVersion,
    expected_version_ctr: i32,
    controller_state_version_change: ConfigVersionChange,
) -> Result<(ExtensionService, ExtensionServiceVersionInfo), DatabaseError> {
    let ConfigVersionChange {
        current: expected_controller_state_version,
        new: next_controller_state_version,
    } = controller_state_version_change;
    let mut builder = sqlx::QueryBuilder::new(
        "UPDATE extension_services SET updated = CURRENT_TIMESTAMP, version_ctr = version_ctr + 1, ",
    );
    builder.push("controller_state = ");
    builder.push_bind(sqlx::types::Json(ExtensionServiceLifecycleState::Updating));
    builder.push(", controller_state_version = ");
    builder.push_bind(next_controller_state_version);
    if let Some(name) = service_name {
        builder.push(", name = ");
        builder.push_bind(name);
    }
    if let Some(desc) = description {
        builder.push(", description = ");
        builder.push_bind(desc);
    }
    builder.push(" WHERE id = ");
    builder.push_bind(service_id);
    builder.push(" AND type = ");
    builder.push_bind(ExtensionServiceType::DpfHelmChart.to_string());
    builder.push(" AND deleted IS NULL AND version_ctr = ");
    builder.push_bind(expected_version_ctr);
    builder.push(" AND controller_state_version = ");
    builder.push_bind(expected_controller_state_version);
    builder.push(" AND controller_state = ");
    builder.push_bind(sqlx::types::Json(ExtensionServiceLifecycleState::Ready));
    builder.push(
        " RETURNING id, type, name, description, tenant_organization_id, version_ctr, \
          controller_state, controller_state_version, controller_state_outcome, created, updated, deleted",
    );

    let updated_service = match builder
        .build_query_as::<ExtensionService>()
        .fetch_one(&mut *txn)
        .await
    {
        Ok(service) => service,
        Err(sqlx::Error::RowNotFound) => {
            return Err(DatabaseError::NotFoundError {
                kind: "active_dpf_helm_chart_extension_service",
                id: service_id.to_string(),
            });
        }
        Err(sqlx::Error::Database(db_err))
            if db_err.is_unique_violation()
                && matches!(
                    db_err.constraint(),
                    Some("extension_services_tenant_lowername_unique")
                        | Some("extension_services_dpf_helm_chart_pending_name_unique")
                )
                && service_name.is_some() =>
        {
            return Err(DatabaseError::AlreadyFoundError {
                kind: "extension_service",
                id: format!("conflict on name {}", service_name.unwrap()),
            });
        }
        Err(error) => return Err(DatabaseError::query(builder.sql(), error)),
    };

    let version_query = "UPDATE extension_service_versions
                         SET data = $1
                         WHERE service_id = $2 AND version = $3 AND deleted IS NULL
                         RETURNING service_id, version, data, observability, has_credential, created, deleted";
    let version = sqlx::query_as::<_, ExtensionServiceVersionInfo>(version_query)
        .bind(normalized_data)
        .bind(service_id)
        .bind(stable_version)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(version_query, error))?;

    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
        &ExtensionServiceLifecycleState::Updating,
        next_controller_state_version,
    )
    .await?;

    Ok((updated_service, version))
}

/// Atomically records the deletion intent for a DPF Helm chart extension
/// service. The DPUService itself is deliberately not touched here: the state
/// controller performs that external operation only after this transaction has
/// committed.
pub async fn request_dpf_helm_chart_deletion(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    stable_version: ConfigVersion,
    expected_state: &ExtensionServiceLifecycleState,
    expected_state_version: ConfigVersion,
) -> DatabaseResult<()> {
    let next_state_version = expected_state_version.increment();
    let service_query = "UPDATE extension_services
                         SET deleted = CURRENT_TIMESTAMP,
                             updated = CURRENT_TIMESTAMP,
                             controller_state = $1::jsonb,
                             controller_state_version = $2
                         WHERE id = $3
                           AND type = $4
                           AND deleted IS NULL
                           AND controller_state = $5::jsonb
                           AND controller_state_version = $6
                         RETURNING id";
    let deleted_service = sqlx::query_scalar::<_, ExtensionServiceId>(service_query)
        .bind(sqlx::types::Json(ExtensionServiceLifecycleState::Deleting))
        .bind(next_state_version)
        .bind(service_id)
        .bind(ExtensionServiceType::DpfHelmChart.to_string())
        .bind(sqlx::types::Json(expected_state))
        .bind(expected_state_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(service_query, error))?;
    if deleted_service.is_none() {
        return Err(DatabaseError::NotFoundError {
            kind: "deletable_dpf_helm_chart_extension_service",
            id: service_id.to_string(),
        });
    }

    let version_query = "UPDATE extension_service_versions
                         SET deleted = CURRENT_TIMESTAMP
                         WHERE service_id = $1 AND version = $2 AND deleted IS NULL
                         RETURNING version";
    let deleted_version = sqlx::query_scalar::<_, ConfigVersion>(version_query)
        .bind(service_id)
        .bind(stable_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(version_query, error))?;
    if deleted_version.is_none() {
        return Err(DatabaseError::NotFoundError {
            kind: "active_dpf_helm_chart_extension_service_version",
            id: format!("{service_id}/{stable_version}"),
        });
    }

    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
        &ExtensionServiceLifecycleState::Deleting,
        next_state_version,
    )
    .await?;

    Ok(())
}

/// `try_update_controller_state` writes the lifecycle state and `new_version`
/// when the version matches `expected_version`.
///
/// A missing service or changed version returns
/// `NotApplied(ControllerStateNotCurrent)` and must not be followed by a history
/// write. `Applied(())` leaves the write in the caller's transaction; database
/// failures remain errors.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &ExtensionServiceLifecycleState,
) -> DatabaseResult<ConditionalWrite<(), ControllerStateNotCurrent>> {
    let query = "UPDATE extension_services
                 SET controller_state_version = $1, controller_state = $2::jsonb
                 WHERE id = $3
                   AND controller_state_version = $4
                 RETURNING id";
    let updated = sqlx::query_scalar::<_, ExtensionServiceId>(query)
        .bind(new_version)
        .bind(sqlx::types::Json(new_state))
        .bind(service_id)
        .bind(expected_version)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match updated {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(ControllerStateNotCurrent),
    })
}

/// Stores the most recent safe controller diagnostic without changing desired
/// lifecycle state or its optimistic-concurrency version.
pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    outcome: PersistentStateHandlerOutcome,
) -> DatabaseResult<()> {
    let query = "UPDATE extension_services
                 SET controller_state_outcome = $1::jsonb
                 WHERE id = $2";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(service_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Finds the IDs of extension services, optionally filtered by type, name, and tenant organization ID.
///
/// # Parameters
/// * `txn`          - A reference to an active DB transaction
/// * `service_type` - Optional filter on the type of the extension service
/// * `service_name` - Optional filter by case-insensitive exact match on service name
/// * `tenant_organization_id` - Optional filter by tenant organization ID
/// * `include_deleted` - Whether soft-deleted services should be included
/// * `for_update`   - A boolean flag to acquire DB locks for synchronization
///
/// # Returns
/// A vector of matching `ExtensionServiceId`s (may be empty).
pub async fn find_ids(
    txn: &mut PgConnection,
    service_type: Option<ExtensionServiceType>,
    service_name: Option<&str>,
    tenant_organization_id: Option<&TenantOrganizationId>,
    include_deleted: bool,
    for_update: bool,
) -> Result<Vec<ExtensionServiceId>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT id FROM extension_services WHERE true");

    if !include_deleted {
        builder.push(" AND deleted IS NULL");
    }

    if let Some(service_type) = service_type {
        builder.push(" AND type = ");
        builder.push_bind(service_type.to_string());
    }

    if let Some(name) = service_name {
        // Extension service name is case-insensitive
        builder
            .push(" AND lower(name) = lower(")
            .push_bind(name)
            .push(")");
    }

    if let Some(tenant_organization_id) = tenant_organization_id {
        builder.push(" AND tenant_organization_id = ");
        builder.push_bind(tenant_organization_id.to_string());
    }

    builder.push(" ORDER BY created DESC");

    if for_update {
        builder.push(" FOR UPDATE");
    }

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Finds extension services by their IDs.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `ids`        - A list of extension service IDs to query
/// * `include_deleted` - Whether soft-deleted services should be included
/// * `for_update` - Whether to lock the extension services for update
pub async fn find_by_ids(
    txn: &mut PgConnection,
    ids: &[ExtensionServiceId],
    include_deleted: bool,
    for_update: bool,
) -> DatabaseResult<Vec<ExtensionService>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = sqlx::QueryBuilder::new(
        "SELECT id, type, name, description, tenant_organization_id, version_ctr,
         controller_state, controller_state_version, controller_state_outcome, created, updated, deleted FROM
         extension_services WHERE id = ANY(",
    );
    builder.push_bind(ids);
    builder.push(")");

    if !include_deleted {
        builder.push(" AND deleted IS NULL");
    }

    if for_update {
        builder.push(" ORDER BY id ");
        builder.push(" FOR UPDATE");
    }

    builder
        .build_query_as::<ExtensionService>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

pub async fn find_snapshots_by_ids(
    txn: &mut PgConnection,
    ids: &[ExtensionServiceId],
) -> DatabaseResult<Vec<ExtensionServiceSnapshot>> {
    // We order the active versions using the version number in descending order
    let query = "WITH versions AS (
        SELECT 
            service_id, version, data, observability, has_credential, created,
            (split_part(split_part(version, '-', 1), 'V', 2))::integer AS version_nr
        FROM extension_service_versions
        WHERE deleted IS NULL AND service_id = ANY($1)
    ),
    agg AS (
        SELECT service_id,
            ARRAY_AGG(version ORDER BY version_nr DESC, created DESC) AS active_versions,
            (ARRAY_AGG(version ORDER BY version_nr DESC, created DESC))[1] AS latest_version
        FROM versions
        GROUP BY service_id
    )
    SELECT
        s.id AS service_id,
        s.name AS service_name,
        s.type AS service_type,
        s.version_ctr AS version_ctr,
        s.description AS description,
        s.tenant_organization_id AS tenant_organization_id,
        s.created AS created,
        s.updated AS updated,
        s.deleted AS deleted,
        s.controller_state AS controller_state,
        s.controller_state_version AS controller_state_version,
        s.controller_state_outcome AS controller_state_outcome,
        COALESCE(a.active_versions, ARRAY[]::varchar[]) AS active_versions,
        a.latest_version AS latest_version,
        v.data as latest_data,
        v.observability as latest_observability,
        v.has_credential as latest_has_credential,
        v.created as latest_created
    FROM extension_services s
    LEFT JOIN agg a ON a.service_id = s.id
    LEFT JOIN versions v ON v.service_id = s.id AND v.version = a.latest_version
    WHERE s.id = ANY($1)
      AND (
          s.deleted IS NULL
          OR s.controller_state ->> 'state' <> 'deleted'
      )
    ORDER BY s.created DESC";

    sqlx::query_as::<_, ExtensionServiceSnapshot>(query)
        .bind(ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Finds a specific version of an extension service, or the latest version if not specified.
/// Returns a NotFoundError if the version is not found.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service
/// * `version`    - Optional specific version number to retrieve. If None, returns the latest version
pub async fn find_version_info(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    version: Option<ConfigVersion>,
) -> DatabaseResult<ExtensionServiceVersionInfo> {
    // We check if the extension service exists first to return a precise service not found error
    let service_query = "SELECT id FROM extension_services WHERE id = $1 AND deleted IS NULL";

    match sqlx::query_scalar::<_, uuid::Uuid>(service_query)
        .bind(service_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(service_query, e))?
    {
        Some(_) => {}
        None => {
            return Err(DatabaseError::NotFoundError {
                kind: "extension_service",
                id: service_id.to_string(),
            });
        }
    }

    find_version_info_of_known_service(txn, service_id, version).await
}

/// Finds a specific version of an extension service, or the latest version if not specified,
/// for callers that have already established the service exists and is not deleted (e.g. via a
/// batched [`find_by_ids`] in the same transaction).
///
/// Unlike [`find_version_info`], this skips the service-existence probe and issues a single
/// query, so an unknown service id surfaces as the version-not-found error rather than the
/// service-not-found error.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service
/// * `version`    - Optional specific version number to retrieve. If None, returns the latest version
pub async fn find_version_info_of_known_service(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    version: Option<ConfigVersion>,
) -> DatabaseResult<ExtensionServiceVersionInfo> {
    // Build the version lookup query.
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT service_id, version, data, observability, has_credential, created, deleted \
         FROM extension_service_versions \
         WHERE deleted IS NULL AND service_id = ",
    );
    builder.push_bind(service_id);

    if let Some(v) = version {
        builder.push(" AND version = ");
        builder.push_bind(v);
    } else {
        builder.push(
            " ORDER BY (split_part(split_part(version, '-', 1), 'V', 2))::integer DESC LIMIT 1",
        );
    }

    let query = builder.build_query_as::<ExtensionServiceVersionInfo>();
    match query.fetch_one(txn).await {
        Ok(ver) => Ok(ver),
        Err(sqlx::Error::RowNotFound) => {
            let id_text = if let Some(v) = version {
                format!("{}/{}", service_id, v)
            } else {
                format!("{}/{}", service_id, "latest")
            };
            Err(DatabaseError::NotFoundError {
                kind: "extension_service_version",
                id: id_text,
            })
        }
        Err(e) => Err(DatabaseError::query(builder.sql(), e)),
    }
}

/// Finds version infos for a given extension service, optionally filtered by version numbers.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service
/// * `versions`   - Optional slice of version numbers to filter by. If None, returns all version infos.
pub async fn find_versions_info(
    txn: &mut PgConnection,
    service_id: &ExtensionServiceId,
    versions: Option<&[ConfigVersion]>,
) -> DatabaseResult<Vec<ExtensionServiceVersionInfo>> {
    // Build the version lookup query.
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT service_id, version, data, observability, has_credential, created, deleted \
     FROM extension_service_versions \
     WHERE deleted IS NULL AND service_id = ",
    );
    builder.push_bind(service_id);

    if let Some(versions) = versions {
        builder.push(" AND version = ANY(");
        builder.push_bind(
            versions
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<String>>(),
        );
        builder.push(")");
    }
    builder.push(" ORDER BY (split_part(split_part(version, '-', 1), 'V', 2))::integer DESC");

    let query = builder.build_query_as::<ExtensionServiceVersionInfo>();
    match query.fetch_all(txn).await {
        Ok(versions) => Ok(versions),
        Err(e) => Err(DatabaseError::query(builder.sql(), e)),
    }
}

/// Finds all non-deleted version numbers for a given extension service.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service
pub async fn find_all_versions(
    txn: impl DbReader<'_>,
    service_id: ExtensionServiceId,
) -> DatabaseResult<Vec<ConfigVersion>> {
    let query = "SELECT version FROM extension_service_versions WHERE deleted IS NULL AND service_id = $1 ORDER BY (split_part(split_part(version, '-', 1), 'V', 2))::integer DESC";

    sqlx::query_scalar::<_, ConfigVersion>(query)
        .bind(service_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Finds all active versions for a given list of extension service IDs, optionally locked the
/// services for update.
///
/// This is a helper function for checking validity of instance extension service configuration.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_ids` - A list of extension service IDs to query
///
/// # Returns
/// A map of extension service IDs to their active versions
pub async fn find_versions_by_service_ids(
    txn: &mut PgConnection,
    service_ids: &[ExtensionServiceId],
    for_update: bool,
) -> DatabaseResult<HashMap<ExtensionServiceId, Vec<ConfigVersion>>> {
    if service_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut builder = sqlx::QueryBuilder::new(
        "SELECT s.id AS service_id, v.version AS version
        FROM extension_services s
        JOIN extension_service_versions v ON s.id = v.service_id
        WHERE s.deleted IS NULL
          AND v.deleted IS NULL
          AND s.id = ANY(",
    );
    builder.push_bind(service_ids);
    builder.push(
        ")
        ORDER BY s.id, (split_part(split_part(v.version, '-', 1), 'V', 2))::integer DESC",
    );
    if for_update {
        builder.push(" FOR UPDATE");
    }

    let versions = builder
        .build_query_as::<(ExtensionServiceId, ConfigVersion)>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?;

    let mut service_versions: HashMap<ExtensionServiceId, Vec<ConfigVersion>> = HashMap::new();
    for (id, version) in versions {
        service_versions.entry(id).or_default().push(version);
    }

    Ok(service_versions)
}

/// Soft deletes an extension service and records the terminal `Deleted`
/// lifecycle transition.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service to soft delete
/// * `expected_controller_state_version` - The current lifecycle version used
///   to atomically record the terminal transition
///
/// # Returns
/// * `Some(service_id)` if the service was successfully soft deleted
/// * `None` if the service is already deleted or not found
/// * `Err` if there is a database error other than RowNotFound
pub async fn soft_delete_service(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    expected_controller_state_version: ConfigVersion,
) -> DatabaseResult<Option<ExtensionServiceId>> {
    let next_controller_state_version = expected_controller_state_version.increment();
    let query = "UPDATE extension_services
            SET deleted = NOW(),
                updated = NOW(),
                controller_state = $1::jsonb,
                controller_state_version = $2,
                controller_state_outcome = NULL
            WHERE id = $3
              AND deleted IS NULL
              AND controller_state_version = $4
            RETURNING id";

    match sqlx::query_as::<_, ExtensionServiceId>(query)
        .bind(sqlx::types::Json(ExtensionServiceLifecycleState::Deleted))
        .bind(next_controller_state_version)
        .bind(service_id)
        .bind(expected_controller_state_version)
        .fetch_one(&mut *txn)
        .await
    {
        Ok(service_id) => {
            crate::state_history::persist(
                txn,
                crate::state_history::StateHistoryTableId::ExtensionService,
                &service_id,
                &ExtensionServiceLifecycleState::Deleted,
                next_controller_state_version,
            )
            .await?;
            Ok(Some(service_id))
        }
        Err(sqlx::Error::RowNotFound) => Ok(None),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

/// Soft deletes specific versions of an extension service by setting their deleted timestamp.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service
/// * `versions`   - Optional slice of version numbers to soft delete, rf empty, all non-deleted
///   versions will be soft deleted.
///
/// # Returns
/// A vector of version numbers that were successfully soft deleted (excluding ones that were
/// already deleted or missing).
pub async fn soft_delete_versions(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    versions: &[ConfigVersion],
) -> DatabaseResult<Vec<ConfigVersion>> {
    let mut builder = sqlx::QueryBuilder::new(
        "UPDATE extension_service_versions SET deleted = NOW() WHERE deleted IS NULL",
    );
    builder.push(" AND service_id = ");
    builder.push_bind(service_id);
    if !versions.is_empty() {
        builder.push(" AND version = ANY(");
        builder.push_bind(
            versions
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<String>>(),
        );
        builder.push(")");
    }
    builder.push(" RETURNING version");

    builder
        .build_query_scalar::<ConfigVersion>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Checks if the extension service is in use by any instance.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service
/// * `versions`   - Optional slice of version numbers to check if the service is in use by any instance
/// * `include_deleted_instances` - Whether to treat a soft-deleted instance
///   whose persisted configuration still refers to the service as in use. Such
///   rows are retained while instance cleanup is still in progress.
pub async fn is_service_in_use(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    versions: &[ConfigVersion],
    include_deleted_instances: bool,
) -> DatabaseResult<bool> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT 1
           FROM instances
          WHERE ",
    );
    if !include_deleted_instances {
        builder.push("deleted IS NULL AND ");
    }
    builder.push(
        "EXISTS (
                 SELECT 1
                   FROM jsonb_array_elements(extension_services_config->'service_configs') AS cfg
                  WHERE cfg->>'service_id' = ",
    );
    builder.push_bind(service_id.to_string());
    builder.push("::text");

    // If filtering by versions, add a version filter
    if !versions.is_empty() {
        builder.push(" AND cfg->>'version' = ANY(");
        builder.push_bind(
            versions
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<String>>(),
        );
        builder.push(")");
    }

    builder.push(") LIMIT 1");

    let exists = builder
        .build_query_scalar::<i32>()
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?
        .is_some();

    Ok(exists)
}

/// Returns the subset of (active) versions of an extension service that have credentials.
///
/// # Parameters
/// * `txn`        - A reference to an active DB transaction
/// * `service_id` - The ID of the extension service
/// * `versions`   - Optional slice of version numbers to check if the service has credentials
pub async fn find_versions_with_credentials(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    versions: &[ConfigVersion],
) -> DatabaseResult<Vec<ConfigVersion>> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT version \
           FROM extension_service_versions \
          WHERE service_id = ",
    );
    builder.push_bind(service_id);
    builder.push(" AND deleted IS NULL AND has_credential = TRUE");

    if !versions.is_empty() {
        builder.push(" AND version = ANY(");
        builder.push_bind(
            versions
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<String>>(),
        );
        builder.push(")");
    }
    builder.push(" ORDER BY (split_part(split_part(version, '-', 1), 'V', 2))::integer DESC");

    builder
        .build_query_scalar::<ConfigVersion>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Set the extension service's updated timestamp.
pub async fn set_updated_timestamp(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
) -> DatabaseResult<()> {
    let query = "UPDATE extension_services SET updated = NOW() \
             WHERE id = $1 AND deleted IS NULL";
    sqlx::query(query)
        .bind(service_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

#[cfg(test)]
mod test_batched_lookups {
    use carbide_test_support::query_counter::count_queries;
    use config_version::ConfigVersion;
    use model::controller_outcome::PersistentStateHandlerOutcome;
    use model::extension_service::{ExtensionServiceLifecycleState, ExtensionServiceType};
    use model::metadata::Metadata;
    use model::tenant::TenantOrganizationId;

    use super::*;

    const TENANT_ORG: &str = "test-org";

    /// Seed N extension services (each with an initial version), returning their ids and the
    /// exact `ConfigVersion` stored for each so tests can look versions up by exact match.
    async fn seed_services(
        pool: &sqlx::PgPool,
        n: usize,
    ) -> Vec<(ExtensionServiceId, ConfigVersion)> {
        let tenant: TenantOrganizationId = TENANT_ORG.parse().expect("valid tenant org id");
        let mut txn = pool.begin().await.expect("begin");

        // Extension services carry an FK to tenants(organization_id); seed the tenant first.
        crate::tenant::create_and_persist(
            TENANT_ORG.to_string(),
            Metadata {
                name: "Test Org".to_string(),
                description: String::new(),
                labels: std::collections::HashMap::new(),
            },
            None,
            &mut txn,
        )
        .await
        .expect("create tenant");

        let mut seeded = Vec::with_capacity(n);
        for i in 0..n {
            let service_id = ExtensionServiceId::new();
            let version = ConfigVersion::initial();
            create(
                &mut txn,
                version,
                &service_id,
                &ExtensionServiceType::KubernetesPod,
                &format!("svc-{i}"),
                &tenant,
                Some("test service"),
                "some-data",
                None,
                false,
            )
            .await
            .expect("create extension service");
            seeded.push((service_id, version));
        }
        txn.commit().await.expect("commit");
        seeded
    }

    #[crate::sqlx_test]
    async fn dpf_helm_chart_controller_persistence_is_versioned_and_retained(pool: sqlx::PgPool) {
        let tenant: TenantOrganizationId = TENANT_ORG.parse().expect("valid tenant org id");
        let service_id = ExtensionServiceId::new();
        let mut txn = pool.begin().await.expect("begin");
        crate::tenant::create_and_persist(
            TENANT_ORG.to_string(),
            Metadata {
                name: "Test Org".to_string(),
                description: String::new(),
                labels: std::collections::HashMap::new(),
            },
            None,
            &mut txn,
        )
        .await
        .expect("create tenant");

        create(
            &mut txn,
            ConfigVersion::initial(),
            &service_id,
            &ExtensionServiceType::DpfHelmChart,
            "dpf-service",
            &tenant,
            Some("DPF Helm chart service"),
            "{\"chart\": \"example\"}",
            None,
            false,
        )
        .await
        .expect("create DPF Helm chart service");

        let creating = find_by_ids(&mut txn, &[service_id], false, false)
            .await
            .map(|mut services| services.pop())
            .expect("load controller record")
            .expect("controller record exists");
        assert_eq!(
            creating.status.controller_state.value,
            ExtensionServiceLifecycleState::Creating
        );
        assert!(creating.status.controller_state_outcome.is_none());
        assert_eq!(creating.status.controller_state.version.version_nr(), 1);

        let initial_history = crate::state_history::for_object(
            &mut txn,
            crate::state_history::StateHistoryTableId::ExtensionService,
            &service_id,
        )
        .await
        .expect("load initial state history");
        assert_eq!(initial_history.len(), 1);
        assert_eq!(
            serde_json::from_str::<ExtensionServiceLifecycleState>(&initial_history[0].state)
                .expect("initial state history is lifecycle JSON"),
            ExtensionServiceLifecycleState::Creating
        );

        let active_version = creating.status.controller_state.version.increment();
        assert_eq!(
            try_update_controller_state(
                &mut txn,
                service_id,
                creating.status.controller_state.version,
                active_version,
                &ExtensionServiceLifecycleState::Ready,
            )
            .await
            .expect("CAS state transition"),
            ConditionalWrite::Applied(())
        );
        crate::state_history::persist(
            &mut txn,
            crate::state_history::StateHistoryTableId::ExtensionService,
            &service_id,
            &ExtensionServiceLifecycleState::Ready,
            active_version,
        )
        .await
        .expect("persist state history");
        assert_eq!(
            try_update_controller_state(
                &mut txn,
                service_id,
                creating.status.controller_state.version,
                active_version.increment(),
                &ExtensionServiceLifecycleState::Failed,
            )
            .await
            .expect("stale CAS is not a database error"),
            ConditionalWrite::NotApplied(ControllerStateNotCurrent)
        );

        let outcome = PersistentStateHandlerOutcome::DoNothing { source_ref: None };
        update_controller_state_outcome(&mut txn, service_id, outcome.clone())
            .await
            .expect("persist controller outcome");
        soft_delete_service(&mut txn, service_id, active_version)
            .await
            .expect("soft delete service");

        assert!(
            find_by_ids(&mut txn, &[service_id], false, false)
                .await
                .expect("exclude soft-deleted controller record")
                .is_empty(),
            "find_by_ids excludes soft-deleted services unless requested"
        );
        let deleted_record = find_by_ids(&mut txn, &[service_id], true, false)
            .await
            .map(|mut services| services.pop())
            .expect("load soft-deleted controller record")
            .expect("soft-deleted controller record is retained");
        assert!(deleted_record.deleted.is_some());
        assert_eq!(
            deleted_record.status.controller_state.value,
            ExtensionServiceLifecycleState::Deleted
        );
        assert_eq!(
            deleted_record.status.controller_state.version.version_nr(),
            active_version.version_nr() + 1
        );
        assert_eq!(deleted_record.status.controller_state_outcome, None);

        let deleted_history = crate::state_history::for_object(
            &mut txn,
            crate::state_history::StateHistoryTableId::ExtensionService,
            &service_id,
        )
        .await
        .expect("load deleted state history");
        assert_eq!(deleted_history.len(), 3);
        assert_eq!(
            deleted_history[2].state_version,
            deleted_record.status.controller_state.version
        );

        let ids = find_ids(
            &mut txn,
            Some(ExtensionServiceType::DpfHelmChart),
            None,
            None,
            true,
            false,
        )
        .await
        .expect("list DPF controller ids");
        assert_eq!(ids, vec![service_id]);
    }

    #[crate::sqlx_test]
    async fn find_by_ids_collapses_n_plus_one(pool: sqlx::PgPool) {
        const N: usize = 8;
        let seeded = seed_services(&pool, N).await;
        let ids = seeded.iter().map(|(id, _)| *id).collect::<Vec<_>>();

        // The reads run on plain pool connections -- no transaction -- so no
        // begin/commit statements land in the counts. Each counted region
        // acquires its connection inside the instrumented future.

        // BEFORE: find_by_ids called with a 1-element slice per service (the pattern in dpu.rs).
        let (looped, before_count) = {
            let pool = &pool;
            let ids = &ids;
            count_queries(async move {
                let mut conn = pool.acquire().await.expect("acquire");
                let mut names = std::collections::HashMap::new();
                for id in ids {
                    let service = find_by_ids(&mut conn, &[*id], false, false)
                        .await
                        .expect("find_by_ids")
                        .into_iter()
                        .next()
                        .expect("service present");
                    names.insert(service.id, service.name);
                }
                names
            })
            .await
        };

        // AFTER: a single find_by_ids over the whole set.
        let (batched, after_count) = {
            let pool = &pool;
            let ids = &ids;
            count_queries(async move {
                let mut conn = pool.acquire().await.expect("acquire");
                find_by_ids(&mut conn, ids, false, false)
                    .await
                    .expect("find_by_ids")
            })
            .await
        };

        // Data equality: same set of (id -> name) pairs.
        assert_eq!(batched.len(), N, "batched returned all N services");
        let batched_names = batched
            .into_iter()
            .map(|service| (service.id, service.name))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            looped, batched_names,
            "batched find_by_ids returns the same id->name mapping as the loop"
        );

        // Bite-check: the loop MUST be more than one query.
        assert!(
            before_count > 1,
            "bite-check failed: looped find_by_ids issued {before_count} queries (expected > 1)"
        );
        assert_eq!(
            before_count, N,
            "looped find_by_ids issues one query per id"
        );
        assert_eq!(
            after_count, 1,
            "batched find_by_ids issues a single query for the whole set"
        );

        println!(
            "extension_service by-id N+1: before(loop find_by_ids)={before_count} after(find_by_ids batch)={after_count} (N={N})"
        );
    }

    #[crate::sqlx_test]
    async fn find_version_info_of_known_service_skips_existence_probe(pool: sqlx::PgPool) {
        let seeded = seed_services(&pool, 1).await;
        let (service_id, version) = seeded[0];

        // The reads run on plain pool connections -- no transaction -- so no
        // begin/commit statements land in the counts. Each counted region
        // acquires its connection inside the instrumented future.

        // find_version_info: existence probe + version lookup.
        let (probed_info, probed_count) = {
            let pool = &pool;
            count_queries(async move {
                let mut conn = pool.acquire().await.expect("acquire");
                find_version_info(&mut conn, service_id, Some(version))
                    .await
                    .expect("find_version_info")
            })
            .await
        };

        // find_version_info_of_known_service: the version lookup alone.
        let (unprobed_info, unprobed_count) = {
            let pool = &pool;
            count_queries(async move {
                let mut conn = pool.acquire().await.expect("acquire");
                find_version_info_of_known_service(&mut conn, service_id, Some(version))
                    .await
                    .expect("find_version_info_of_known_service")
            })
            .await
        };

        // Data equality: both lookups return the same version row.
        assert_eq!(
            (
                probed_info.service_id,
                probed_info.version,
                probed_info.data,
                probed_info.observability,
                probed_info.has_credential,
                probed_info.created,
            ),
            (
                unprobed_info.service_id,
                unprobed_info.version,
                unprobed_info.data,
                unprobed_info.observability,
                unprobed_info.has_credential,
                unprobed_info.created,
            ),
            "both lookups return the same version info"
        );

        assert_eq!(
            probed_count, 2,
            "find_version_info issues two queries (existence probe + version lookup)"
        );
        assert_eq!(
            unprobed_count, 1,
            "find_version_info_of_known_service issues the version lookup alone"
        );

        println!(
            "extension_service version lookup: find_version_info={probed_count} \
             find_version_info_of_known_service={unprobed_count}"
        );
    }
}
