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
use ::rpc::forge as rpc;
use db::resource_pool::{ResourcePoolAllocationNotOwned, ResourcePoolDatabaseError};
use db::{ConditionalWrite, ObjectColumnFilter, WithTransaction, spx_partition};
use futures_util::FutureExt;
use model::resource_pool;
use model::spx_partition::NewSpxPartition;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_tenant_organization_id};

fn emit_and_map_dpa_vni_allocation_failure(
    source_pool: &resource_pool::ResourcePool<i32>,
    owner_id: &str,
    requested_vni: Option<i32>,
    error: ResourcePoolDatabaseError,
) -> CarbideError {
    match (error, requested_vni) {
        (
            error
            @ ResourcePoolDatabaseError::ResourcePool(resource_pool::ResourcePoolError::Empty),
            requested_vni,
        ) => {
            db::resource_pool::emit_allocation_failure(
                source_pool.value_type,
                owner_id,
                requested_vni.is_some(),
                source_pool.name(),
                &error,
            );
            CarbideError::ResourceExhausted(format!("pool {}", source_pool.name))
        }
        (error, Some(requested_vni))
            if db::resource_pool::is_requested_value_unavailable(&error) =>
        {
            db::resource_pool::emit_requested_vni_unavailable(
                source_pool.value_type,
                owner_id,
                requested_vni,
                source_pool.name(),
            );
            CarbideError::FailedPrecondition(format!(
                "VNI `{requested_vni}` cannot be requested or is already allocated"
            ))
        }
        (ResourcePoolDatabaseError::Database(error), Some(_)) => {
            db::resource_pool::emit_database_allocation_failure(
                source_pool.value_type,
                owner_id,
                true,
                source_pool.name(),
                &error,
            );
            (*error).into()
        }
        (error, requested_vni) => {
            db::resource_pool::emit_allocation_failure(
                source_pool.value_type,
                owner_id,
                requested_vni.is_some(),
                source_pool.name(),
                &error,
            );
            error.into()
        }
    }
}

async fn allocate_dpa_vni(
    api: &Api,
    txn: &mut PgConnection,
    owner_id: &str,
    requested_vni: Option<i32>,
) -> Result<i32, CarbideError> {
    let source_pool = &api.common_pools.ethernet.pool_dpa_vni;

    db::resource_pool::allocate(
        source_pool,
        txn,
        resource_pool::OwnerType::SpxPartition,
        owner_id,
        requested_vni,
    )
    .await
    .map_err(|error| {
        emit_and_map_dpa_vni_allocation_failure(source_pool, owner_id, requested_vni, error)
    })
}

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::SpxPartitionCreationRequest>,
) -> Result<Response<rpc::SpxPartition>, Status> {
    log_request_data(&request);

    let request_inner = request.into_inner();
    log_tenant_organization_id(&request_inner.tenant_organization_id);

    let req = NewSpxPartition::try_from(request_inner)?;

    let mut txn = api.txn_begin().await?;

    let vni = allocate_dpa_vni(api, &mut txn, &req.id.to_string(), req.vni).await?;

    let partition = db::spx_partition::create(&req, vni, &mut txn)
        .await
        .map_err(CarbideError::from)?;
    let resp = rpc::SpxPartition::try_from(partition).map(Response::new)?;
    txn.commit().await?;
    Ok(resp)
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::SpxPartitionDeletionRequest>,
) -> Result<Response<rpc::SpxPartitionDeletionResult>, Status> {
    log_request_data(&request);

    let id = request
        .into_inner()
        .id
        .ok_or_else(|| CarbideError::MissingArgument("id"))?;

    let resp = api
        .with_txn(|txn| db::spx_partition::mark_as_deleted(id, txn).boxed())
        .await?
        .map_err(CarbideError::from)?;

    if let Some(vni) = resp.vni {
        let mut txn = api.txn_begin().await?;

        // A free or reassigned VNI leaves this partition nothing to release.
        match db::resource_pool::release(
            &api.common_pools.ethernet.pool_dpa_vni,
            &mut txn,
            vni,
            resource_pool::OwnerType::SpxPartition,
            &id.to_string(),
        )
        .await
        .map_err(CarbideError::from)?
        {
            ConditionalWrite::Applied(())
            | ConditionalWrite::NotApplied(ResourcePoolAllocationNotOwned) => {}
        }

        txn.commit().await?;
    }

    Ok(Response::new(rpc::SpxPartitionDeletionResult {}))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::SpxPartitionSearchFilter>,
) -> Result<Response<rpc::SpxPartitionIdList>, Status> {
    log_request_data(&request);

    let rpc_filter = request.into_inner();
    if let Some(ref tenant_org_id) = rpc_filter.tenant_org_id {
        log_tenant_organization_id(tenant_org_id);
    }

    let filter: model::spx_partition::SpxPartitionSearchFilter = rpc_filter.into();
    let spx_partition_ids = db::spx_partition::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::SpxPartitionIdList { spx_partition_ids }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::SpxPartitionsByIdsRequest>,
) -> Result<Response<rpc::SpxPartitionList>, Status> {
    log_request_data(&request);

    let rpc::SpxPartitionsByIdsRequest {
        spx_partition_ids, ..
    } = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if spx_partition_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if spx_partition_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut reader = api.db_reader();
    let partitions = db::spx_partition::find_by(
        reader.as_mut(),
        ObjectColumnFilter::List(spx_partition::IdColumn, &spx_partition_ids),
    )
    .await
    .map_err(CarbideError::from)?;

    let mut spx_partitions = Vec::with_capacity(partitions.len());
    for p in partitions {
        spx_partitions.push(p.try_into()?);
    }

    Ok(Response::new(rpc::SpxPartitionList { spx_partitions }))
}

#[cfg(test)]
mod tests {
    use ::rpc::forge as rpc;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};
    use db::resource_pool::ResourcePoolDatabaseError;
    use model::resource_pool::common::DPA_VNI;
    use model::resource_pool::{ResourcePool, ResourcePoolError, ValueType};
    use model::spx_partition::NewSpxPartition;
    use tonic::Code;

    use super::emit_and_map_dpa_vni_allocation_failure;

    const RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC: &str =
        "carbide_resource_pool_lifecycle_failures_total";
    const OWNER_ID: &str = "spx-1";
    const PARSE_ERROR: &str = concat!(
        "cannot convert 'not-an-integer' to dpa-vni's pool type for ",
        "spx_partition spx-1: invalid integer"
    );

    #[derive(Debug, Clone, Copy)]
    enum DpaVniFailureInput {
        Exhausted,
        RequestedUnavailable,
        RequestedDatabase,
        Generic,
    }

    impl DpaVniFailureInput {
        fn error(self) -> ResourcePoolDatabaseError {
            match self {
                Self::Exhausted => ResourcePoolError::Empty.into(),
                Self::RequestedUnavailable => db::DatabaseError::FailedPrecondition(
                    "requested VNI is unavailable".to_string(),
                )
                .into(),
                Self::RequestedDatabase => db::DatabaseError::Internal {
                    message: "database unavailable".to_string(),
                }
                .into(),
                Self::Generic => ResourcePoolError::Parse {
                    e: "invalid integer".to_string(),
                    v: "not-an-integer".to_string(),
                    pool_name: DPA_VNI.to_string(),
                    owner_type: "spx_partition".to_string(),
                    owner_id: OWNER_ID.to_string(),
                }
                .into(),
            }
        }

        fn failure_label(self) -> &'static str {
            match self {
                Self::Exhausted => "exhausted",
                Self::RequestedUnavailable => "requested_value_unavailable",
                Self::RequestedDatabase => "database",
                Self::Generic => "parse",
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct DpaVniFailureCase {
        failure: DpaVniFailureInput,
        requested_vni: Option<i32>,
    }

    impl DpaVniFailureCase {
        fn allocation_mode(self) -> &'static str {
            if self.requested_vni.is_some() {
                "requested"
            } else {
                "automatic"
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct DpaVniFailureObservation {
        status_code: Code,
        status_message: String,
        metadata_name: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        operation: Option<String>,
        failure: Option<String>,
        failure_policy: Option<String>,
        allocation_mode: Option<String>,
        value_type: Option<String>,
        owner_id: Option<String>,
        pool: Option<String>,
        requested_vni: Option<String>,
        error: Option<String>,
        counter_delta: f64,
    }

    #[test]
    fn dpa_vni_failure_mapper_preserves_status_and_event_contract() {
        check_values(
            [
                Check {
                    scenario: "pool exhaustion",
                    input: DpaVniFailureCase {
                        failure: DpaVniFailureInput::Exhausted,
                        requested_vni: None,
                    },
                    expect: DpaVniFailureObservation {
                        status_code: Code::ResourceExhausted,
                        status_message: "pool dpa-vni".to_string(),
                        metadata_name: "resource_pool_exhausted".to_string(),
                        level: tracing::Level::ERROR,
                        message: "Pool exhausted, cannot allocate".to_string(),
                        event_name: Some("resource_pool_exhausted".to_string()),
                        metric_name: Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC.to_string()),
                        operation: Some("allocate".to_string()),
                        failure: Some("exhausted".to_string()),
                        failure_policy: Some("required".to_string()),
                        allocation_mode: Some("automatic".to_string()),
                        value_type: Some("integer".to_string()),
                        owner_id: Some(OWNER_ID.to_string()),
                        pool: Some(DPA_VNI.to_string()),
                        requested_vni: None,
                        error: None,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "requested VNI unavailable",
                    input: DpaVniFailureCase {
                        failure: DpaVniFailureInput::RequestedUnavailable,
                        requested_vni: Some(42),
                    },
                    expect: DpaVniFailureObservation {
                        status_code: Code::FailedPrecondition,
                        status_message: "VNI `42` cannot be requested or is already allocated"
                            .to_string(),
                        metadata_name: "resource_pool_requested_vni_unavailable".to_string(),
                        level: tracing::Level::ERROR,
                        message: "invalid pool value requested, cannot allocate".to_string(),
                        event_name: Some("resource_pool_requested_vni_unavailable".to_string()),
                        metric_name: Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC.to_string()),
                        operation: Some("allocate".to_string()),
                        failure: Some("requested_value_unavailable".to_string()),
                        failure_policy: Some("required".to_string()),
                        allocation_mode: Some("requested".to_string()),
                        value_type: Some("integer".to_string()),
                        owner_id: Some(OWNER_ID.to_string()),
                        pool: Some(DPA_VNI.to_string()),
                        requested_vni: Some("42".to_string()),
                        error: None,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "requested VNI database failure",
                    input: DpaVniFailureCase {
                        failure: DpaVniFailureInput::RequestedDatabase,
                        requested_vni: Some(42),
                    },
                    expect: DpaVniFailureObservation {
                        status_code: Code::Internal,
                        status_message: "internal error: database unavailable".to_string(),
                        metadata_name: "resource_pool_allocation_failed".to_string(),
                        level: tracing::Level::ERROR,
                        message: "Error allocating from resource pool".to_string(),
                        event_name: Some("resource_pool_allocation_failed".to_string()),
                        metric_name: Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC.to_string()),
                        operation: Some("allocate".to_string()),
                        failure: Some("database".to_string()),
                        failure_policy: Some("required".to_string()),
                        allocation_mode: Some("requested".to_string()),
                        value_type: Some("integer".to_string()),
                        owner_id: Some(OWNER_ID.to_string()),
                        pool: Some(DPA_VNI.to_string()),
                        requested_vni: None,
                        error: Some("internal error: database unavailable".to_string()),
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "generic allocation failure",
                    input: DpaVniFailureCase {
                        failure: DpaVniFailureInput::Generic,
                        requested_vni: None,
                    },
                    expect: DpaVniFailureObservation {
                        status_code: Code::Internal,
                        status_message: format!("resource pool database error: {PARSE_ERROR}"),
                        metadata_name: "resource_pool_allocation_failed".to_string(),
                        level: tracing::Level::ERROR,
                        message: "Error allocating from resource pool".to_string(),
                        event_name: Some("resource_pool_allocation_failed".to_string()),
                        metric_name: Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC.to_string()),
                        operation: Some("allocate".to_string()),
                        failure: Some("parse".to_string()),
                        failure_policy: Some("required".to_string()),
                        allocation_mode: Some("automatic".to_string()),
                        value_type: Some("integer".to_string()),
                        owner_id: Some(OWNER_ID.to_string()),
                        pool: Some(DPA_VNI.to_string()),
                        requested_vni: None,
                        error: Some(PARSE_ERROR.to_string()),
                        counter_delta: 1.0,
                    },
                },
            ],
            |input| {
                let source_pool = ResourcePool::<i32>::new(DPA_VNI.to_string(), ValueType::Integer);
                let metrics = MetricsCapture::start();
                let mut mapped_error = None;
                let logs = capture_logs(|| {
                    mapped_error = Some(emit_and_map_dpa_vni_allocation_failure(
                        &source_pool,
                        OWNER_ID,
                        input.requested_vni,
                        input.failure.error(),
                    ));
                });
                let status =
                    tonic::Status::from(mapped_error.expect("failure mapper returns an error"));
                assert_eq!(logs.len(), 1);
                let log = &logs[0];

                DpaVniFailureObservation {
                    status_code: status.code(),
                    status_message: status.message().to_string(),
                    metadata_name: log.metadata_name.clone(),
                    level: log.level,
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    operation: log.field("operation").map(str::to_string),
                    failure: log.field("failure").map(str::to_string),
                    failure_policy: log.field("failure_policy").map(str::to_string),
                    allocation_mode: log.field("allocation_mode").map(str::to_string),
                    value_type: log.field("value_type").map(str::to_string),
                    owner_id: log.field("owner_id").map(str::to_string),
                    pool: log.field("pool").map(str::to_string),
                    requested_vni: log.field("requested_vni").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    counter_delta: metrics.counter_delta(
                        RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC,
                        &[
                            ("operation", "allocate"),
                            ("failure", input.failure.failure_label()),
                            ("failure_policy", "required"),
                            ("allocation_mode", input.allocation_mode()),
                            ("value_type", "integer"),
                        ],
                    ),
                }
            },
        );
    }

    #[test]
    fn test_create_spx_partition_valid_request() {
        let request = rpc::SpxPartitionCreationRequest {
            metadata: Some(rpc::Metadata {
                name: "test-partition".to_string(),
                description: "A test SPX partition".to_string(),
                ..Default::default()
            }),
            id: None,
            vni: Some(100),
            tenant_organization_id: "tenant-org-123".to_string(),
        };

        let result = NewSpxPartition::try_from(request);
        assert!(result.is_ok());

        let partition = result.unwrap();
        assert_eq!(partition.name, "test-partition");
        assert_eq!(partition.description, "A test SPX partition");
        assert_eq!(partition.tenant_organization_id, "tenant-org-123");
        assert_eq!(partition.vni, Some(100));
    }

    #[test]
    fn test_create_spx_partition_missing_tenant_organization_id() {
        let request = rpc::SpxPartitionCreationRequest {
            metadata: Some(rpc::Metadata {
                name: "test-partition".to_string(),
                description: "A test SPX partition".to_string(),
                ..Default::default()
            }),
            id: None,
            vni: Some(100),
            tenant_organization_id: String::new(),
        };

        let result = NewSpxPartition::try_from(request);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.to_string().contains("tenant_organization_id"));
    }

    #[test]
    fn test_create_spx_partition_auto_generates_id() {
        let request = rpc::SpxPartitionCreationRequest {
            metadata: Some(rpc::Metadata {
                name: "test-partition".to_string(),
                description: "".to_string(),
                ..Default::default()
            }),
            id: None,
            vni: None,
            tenant_organization_id: "tenant-org-123".to_string(),
        };

        let result = NewSpxPartition::try_from(request);
        assert!(result.is_ok());

        let partition = result.unwrap();
        assert_ne!(partition.id, carbide_uuid::spx::SpxPartitionId::nil());
    }

    #[test]
    fn test_create_spx_partition_uses_provided_id() {
        let provided_id: carbide_uuid::spx::SpxPartitionId = uuid::Uuid::new_v4().into();

        let request = rpc::SpxPartitionCreationRequest {
            metadata: Some(rpc::Metadata {
                name: "test-partition".to_string(),
                description: "".to_string(),
                ..Default::default()
            }),
            id: Some(provided_id),
            vni: None,
            tenant_organization_id: "tenant-org-123".to_string(),
        };

        let result = NewSpxPartition::try_from(request);
        assert!(result.is_ok());

        let partition = result.unwrap();
        assert_eq!(partition.id, provided_id);
    }

    #[test]
    fn test_create_spx_partition_without_metadata() {
        let request = rpc::SpxPartitionCreationRequest {
            metadata: None,
            id: None,
            vni: Some(200),
            tenant_organization_id: "tenant-org-456".to_string(),
        };

        let result = NewSpxPartition::try_from(request);
        assert!(result.is_ok());

        let partition = result.unwrap();
        assert!(partition.name.is_empty());
        assert!(partition.description.is_empty());
        assert_eq!(partition.vni, Some(200));
    }

    #[test]
    fn test_create_spx_partition_without_vni() {
        let request = rpc::SpxPartitionCreationRequest {
            metadata: Some(rpc::Metadata {
                name: "no-vni-partition".to_string(),
                description: "".to_string(),
                ..Default::default()
            }),
            id: None,
            vni: None,
            tenant_organization_id: "tenant-org-789".to_string(),
        };

        let result = NewSpxPartition::try_from(request);
        assert!(result.is_ok());

        let partition = result.unwrap();
        assert!(partition.vni.is_none());
    }
}
