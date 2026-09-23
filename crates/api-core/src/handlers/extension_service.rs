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
use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge as rpc;
use carbide_instrument::{Event, LabelValue, emit};
use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::ConfigVersion;
use db::{WithTransaction, extension_service, instance};
use futures_util::FutureExt;
use model::extension_service::{
    DpfHelmChartServiceData, ExtensionService, ExtensionServiceLifecycleState,
    ExtensionServiceObservability, ExtensionServiceType, ExtensionServiceVersionInfo,
    ServiceVpcInterfaceRequirement,
};
use model::tenant::TenantOrganizationId;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_tenant_organization_id};

const MAX_DATA_SIZE: usize = 2 << 16; // 128 KB
const MAX_OBSERVABILITY_CONFIG_PER_SERVICE: usize = 20;

/// Which API operation left an extension-service credential for cleanup.
///
/// `operation` is the only metric label. Service IDs, versions, and errors
/// stay on the log record so individual credentials cannot create new series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum ExtensionServiceCredentialCleanupOperation {
    Create,
    Update,
    Delete,
}

/// A stored extension-service credential outlived the record it belonged to,
/// and the follow-up cleanup could not delete it. Each variant is the API call
/// that left it behind; only a delete removed a specific version, so only it
/// holds one.
#[derive(Event)]
#[event(
    event_name = "extension_service_credential_cleanup_failed",
    metric_name = "carbide_extension_service_credential_cleanup_failures_total",
    component = "nico-api",
    metric = counter,
    log = warn,
    describe = "Number of extension-service credential cleanup failures, by operation.",
    labels(operation: ExtensionServiceCredentialCleanupOperation),
)]
enum ExtensionServiceCredentialCleanupFailed {
    #[event(
        labels(operation = Create),
        message = "Failed to delete extension service credential after transaction failure"
    )]
    Create {
        #[context]
        extension_service_id: ExtensionServiceId,
        #[context]
        error: String,
    },

    #[event(
        labels(operation = Update),
        message = "Failed to delete extension service credential after transaction failure"
    )]
    Update {
        #[context]
        extension_service_id: ExtensionServiceId,
        #[context]
        error: String,
    },

    #[event(
        labels(operation = Delete),
        message = "Failed to delete extension service credential"
    )]
    Delete {
        #[context]
        extension_service_id: ExtensionServiceId,
        #[context]
        version: ConfigVersion,
        #[context]
        error: String,
    },
}

/// Validates and converts the complete service-facing interface definition.
fn service_vpc_interfaces_from_rpc(
    service_type: &ExtensionServiceType,
    requirements: &[rpc::ServiceVpcInterfaceRequirement],
) -> Result<Vec<ServiceVpcInterfaceRequirement>, CarbideError> {
    // The MVP maps at most one registered interface to one VPC selection.
    if requirements.len() > 1 {
        return Err(CarbideError::InvalidArgument(
            "at most one service VPC interface is supported".to_string(),
        ));
    }
    let requirements = requirements
        .iter()
        .copied()
        .map(ServiceVpcInterfaceRequirement::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(CarbideError::from)?;

    // Only a DPF-managed workload can run inside the DPU-local service VRF.
    if !requirements.is_empty() && service_type != &ExtensionServiceType::DpfHelmChart {
        return Err(CarbideError::InvalidArgument(
            "service VPC interfaces are supported only for DPF helm chart extension services"
                .to_string(),
        ));
    }

    Ok(requirements)
}

/// Creates a new extension service with an initial version.
pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateDpuExtensionServiceRequest>,
) -> Result<Response<rpc::DpuExtensionService>, Status> {
    // Do not log_request_data as request may contain credential or sensitive data
    tracing::Span::current().record("request", "CreateDpuExtensionServiceRequest { }");

    let req = request.into_inner();

    let service_id = match req.service_id {
        Some(id) => id.parse::<ExtensionServiceId>().map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidUuid(
                "ExtensionServiceId",
                e.to_string(),
            ))
        })?,
        None => ExtensionServiceId::from(Uuid::new_v4()),
    };

    log_tenant_organization_id(&req.tenant_organization_id);

    let tenant_organization_id = req
        .tenant_organization_id
        .parse::<TenantOrganizationId>()
        .map_err(|e| CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string())))?;

    // Validate required fields
    if req.service_name.is_empty() {
        return Err(CarbideError::MissingArgument("service_name").into());
    }
    let service_type: ExtensionServiceType =
        rpc::DpuExtensionServiceType::try_from(req.service_type)
            .map_err(|_| CarbideError::InvalidArgument("invalid service_type".to_string()))?
            .into();

    let service_vpc_interfaces =
        service_vpc_interfaces_from_rpc(&service_type, &req.service_vpc_interfaces)?;
    let dpu_target = match (&service_type, req.dpu_target) {
        (ExtensionServiceType::KubernetesPod, None) => None,
        (ExtensionServiceType::DpfHelmChart, Some(value)) => Some(
            rpc::DpuExtensionServiceDpuTarget::try_from(value)
                .map_err(|_| {
                    CarbideError::InvalidArgument(
                        "dpu_target must be PRIMARY, ALL_ACTIVE, or ALL for helm services".into(),
                    )
                })?
                .into(),
        ),
        (ExtensionServiceType::DpfHelmChart, None) => {
            return Err(CarbideError::MissingArgument("dpu_target").into());
        }
        (ExtensionServiceType::KubernetesPod, Some(_)) => {
            return Err(CarbideError::InvalidArgument(
                "dpu_target is unsupported for kubernetes pod services".into(),
            )
            .into());
        }
    };

    let initial_version = ConfigVersion::initial();

    // Validate service type is supported by the site
    validate_extension_service_type_enabled(&service_type, api.runtime_config.dpf.enabled)?;

    // Validate the complete service definition before writing anything.
    // Kubernetes Pod data is stored exactly as provided; DPF Helm chart data
    // is parsed before becoming durable desired state.
    validate_extension_service_data_size(&req.data)?;
    let data = match &service_type {
        ExtensionServiceType::KubernetesPod => {
            validate_pod_spec_file(&req.data)?;
            req.data
        }
        ExtensionServiceType::DpfHelmChart => parse_dpf_helm_chart_data(&req.data)?,
    };

    // @TODO(Felicity): support observability for DpfHelmChart extension services
    if matches!(service_type, ExtensionServiceType::DpfHelmChart) && req.observability.is_some() {
        return Err(CarbideError::FailedPrecondition(
            "observability configuration for DPF helm chart extension services is not supported yet"
                .to_string(),
        )
        .into());
    }
    let obvs_len = req
        .observability
        .as_ref()
        .map(|o| o.configs.len())
        .unwrap_or(0);
    if obvs_len > MAX_OBSERVABILITY_CONFIG_PER_SERVICE {
        return Err(CarbideError::InvalidConfiguration(
            model::ConfigValidationError::InvalidValue(format!(
                "{} configured observability configs for extension service exceeds the limit of {MAX_OBSERVABILITY_CONFIG_PER_SERVICE}",
                obvs_len
            )),
        ).into());
    }
    let observability = req
        .observability
        .map(ExtensionServiceObservability::try_from)
        .transpose()?;

    // Store the new credential in Vault if provided. We have to do this before updating the data in
    // the database, so that in case this fails, the database remains untouched. We can't use db
    // transactions for this since it can cause issues if vault is unresponsive.
    if let Some(credential) = &req.credential {
        validate_extension_service_credential(&service_type, credential)?;
        create_extension_service_credential(
            &service_type,
            &api.credential_manager,
            create_extension_service_credential_key(&service_id, initial_version),
            credential,
        )
        .await?;
    }

    // Finally, create the extension in the database. If this fails, the vault credential will be removed.
    let (service, version) = match api
        .with_txn(|txn| {
            async {
                extension_service::create(
                    txn,
                    initial_version,
                    &service_id,
                    &service_type,
                    dpu_target,
                    &req.service_name,
                    &tenant_organization_id,
                    req.description.as_deref(),
                    &service_vpc_interfaces,
                    &data,
                    observability,
                    req.credential.is_some(),
                )
                .await
            }
            .boxed()
        })
        .await
    {
        Ok(Ok(result)) => result,
        Err(e) | Ok(Err(e)) => {
            // If creating the extension fails, we need to delete the credential stored in Vault.
            if req.credential.is_some() {
                let credential_key =
                    create_extension_service_credential_key(&service_id, initial_version);
                // Cleanup is best effort: keep the transaction error as the
                // request error, but record any credential it leaves behind.
                if let Err(delete_err) =
                    delete_extension_service_credential(&api.credential_manager, credential_key)
                        .await
                {
                    emit(ExtensionServiceCredentialCleanupFailed::Create {
                        extension_service_id: service_id,
                        error: delete_err.to_string(),
                    });
                }
            }
            return Err(e.into());
        }
    };

    let lifecycle_status = ::rpc::model::extension_service::lifecycle_status(
        service.status.controller_state.value,
        service.status.controller_state.version,
        service.status.controller_state_outcome.clone(),
    );

    // Create response with service details
    let response = rpc::DpuExtensionService {
        service_id: service.id.to_string(),
        dpu_target: service
            .dpu_target
            .map(|target| rpc::DpuExtensionServiceDpuTarget::from(target) as i32),
        service_type: rpc::DpuExtensionServiceType::from(service_type) as i32,
        service_name: service.name,
        tenant_organization_id: service.tenant_organization_id.to_string(),
        version_ctr: service.version_ctr,
        active_versions: vec![version.version.to_string()],
        latest_version_info: Some(version.into()),
        description: service.description,
        created: service.created.to_string(),
        updated: service.updated.to_string(),
        lifecycle_status: Some(lifecycle_status),
        service_vpc_interfaces: service
            .service_vpc_interfaces
            .into_iter()
            .map(Into::into)
            .collect(),
    };

    Ok(Response::new(response))
}

/// Updates an existing extension service.
///
/// Metadata-only updates do not create a version. DPF Helm chart updates mutate
/// the stable V1 definition for asynchronous controller reconciliation, whereas
/// Kubernetes Pod updates create a new version and may update Vault credentials.
/// Interface requirements are replaced only when no durable attachment exists.
pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateDpuExtensionServiceRequest>,
) -> Result<Response<rpc::DpuExtensionService>, Status> {
    // Do not log_request_data as request may contain credential or sensitive data
    tracing::Span::current().record("request", "UpdateDpuExtensionServiceRequest { }");

    let req = request.into_inner();

    let service_id = req.service_id.parse::<ExtensionServiceId>().map_err(|e| {
        CarbideError::from(RpcDataConversionError::InvalidUuid(
            "ExtensionServiceId",
            e.to_string(),
        ))
    })?;

    if req.service_name.is_some() && req.service_name.as_ref().unwrap().is_empty() {
        return Err(
            CarbideError::InvalidArgument("service_name cannot be empty".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    // We lock the extension service for update so that no other request can update the service
    let current_service_res =
        extension_service::find_by_ids(&mut txn, &[service_id], false, true).await?;
    let current_service = match current_service_res.len() {
        0 => {
            return Err(CarbideError::NotFoundError {
                kind: "extension_service",
                id: service_id.to_string(),
            }
            .into());
        }
        1 => current_service_res.first().unwrap(),
        _ => {
            return Err(CarbideError::Internal {
                message: "Multiple extension services found for the same ID".to_string(),
            }
            .into());
        }
    };

    // If the if_version_ctr_match is provided, check if the current version matches the provided version
    if let Some(version_ctr) = req.if_version_ctr_match
        && current_service.version_ctr != version_ctr
    {
        return Err(CarbideError::ConcurrentModificationError(
            "ExtensionService",
            version_ctr.to_string(),
        )
        .into());
    }

    let service_vpc_interfaces = match req.service_vpc_interfaces.as_ref() {
        // A present wrapper is the complete desired definition (including an
        // explicitly empty definition that removes all requirements).
        Some(requirements) => service_vpc_interfaces_from_rpc(
            &current_service.service_type,
            &requirements.interfaces,
        )?,
        // Pre-feature clients omit this field. When nothing is stored, that
        // omission and an explicitly empty definition mean the same thing.
        None if current_service.service_vpc_interfaces.is_empty() => Vec::new(),
        // Reusing the stored list here would turn a whole-definition update
        // into a patch and could hide a caller that omitted required input.
        None => {
            return Err(CarbideError::InvalidArgument(
                "service_vpc_interfaces must be provided when updating an extension service with registered interface requirements"
                    .to_string(),
            )
            .into());
        }
    };
    let service_vpc_interfaces_changed =
        service_vpc_interfaces != current_service.service_vpc_interfaces;

    // Requirements describe every attachment of the service, including entries
    // still terminating on deleted instances. The service row lock serializes
    // this check with attachment creation, whose validation takes the same lock.
    if service_vpc_interfaces_changed
        && extension_service::is_service_in_use(&mut txn, service_id, &[], true).await?
    {
        return Err(CarbideError::FailedPrecondition(
            "service VPC interface requirements cannot be changed while the extension service has active or terminating attachments"
                .to_string(),
        )
        .into());
    }

    // Metadata-only updates preserve the complete registered definition.
    let metadata_only = !service_vpc_interfaces_changed
        && req.data.is_empty()
        && req.credential.is_none()
        && req.observability.is_none()
        && (req.service_name.as_deref().is_some_and(|s| !s.is_empty())
            || req.description.is_some());

    let (updated_service, latest_version_row) = if metadata_only {
        // The name and description are updated in the database if provided, but no new version is
        // created.
        let updated_service = extension_service::update_metadata(
            &mut txn,
            service_id,
            req.service_name.as_deref(),
            req.description.as_deref(),
        )
        .await?;

        let latest_version_row =
            extension_service::find_version_info(&mut txn, service_id, None).await?;
        txn.commit().await?;

        (updated_service, latest_version_row)
    } else {
        match &current_service.service_type {
            ExtensionServiceType::DpfHelmChart => {
                let result = update_dpf_helm_chart(
                    &mut txn,
                    service_id,
                    current_service,
                    &service_vpc_interfaces,
                    &req,
                )
                .await?;
                txn.commit().await?;
                result
            }
            ExtensionServiceType::KubernetesPod => {
                update_kubernetes_pod(api, txn, service_id, current_service, req).await?
            }
        }
    };

    updated_extension_service_response(api, service_id, updated_service, latest_version_row).await
}

/// Updates the mutable V1 definition of a DPF Helm chart service and requests
/// asynchronous reconciliation of its stable DPUService.
async fn update_dpf_helm_chart(
    txn: &mut db::Transaction<'_>,
    service_id: ExtensionServiceId,
    current_service: &ExtensionService,
    service_vpc_interfaces: &[ServiceVpcInterfaceRequirement],
    req: &rpc::UpdateDpuExtensionServiceRequest,
) -> Result<(ExtensionService, ExtensionServiceVersionInfo), Status> {
    if req.credential.is_some() {
        return Err(CarbideError::FailedPrecondition(
            "credentials for DPF helm chart extension services are not supported through API, they should be preprovisioned in site".to_string(),
        )
        .into());
    }
    if req.observability.is_some() {
        return Err(CarbideError::FailedPrecondition(
            "observability configuration for DPF helm chart extension services is not supported yet"
                .to_string(),
        )
        .into());
    }
    if current_service.status.controller_state.value != ExtensionServiceLifecycleState::Ready {
        return Err(CarbideError::FailedPrecondition(format!(
            "DPF helm chart extension service data can only be updated while ready; current state is {:?}",
            current_service.status.controller_state.value
        ))
        .into());
    }

    validate_extension_service_data_size(&req.data)?;
    let parsed_data = parse_dpf_helm_chart_data(&req.data)?;
    let existing_v1 =
        extension_service::find_version_info_of_known_service(txn, service_id, None).await?;
    let parsed_existing = parse_dpf_helm_chart_data(&existing_v1.data)?;
    if parsed_data == parsed_existing
        && service_vpc_interfaces == current_service.service_vpc_interfaces
    {
        return Err(CarbideError::InvalidArgument(
            "no changes to data or service VPC interfaces from the current DPF helm chart definition"
                .to_string(),
        )
        .into());
    }
    let controller_state_version_change = current_service
        .status
        .controller_state
        .version
        .incremental_change();

    Ok(extension_service::update_dpf_helm_chart_in_place(
        txn,
        service_id,
        req.service_name.as_deref(),
        req.description.as_deref(),
        service_vpc_interfaces,
        &parsed_data,
        existing_v1.version,
        current_service.version_ctr,
        controller_state_version_change,
    )
    .await?)
}

/// Creates a new Kubernetes Pod service version after validating any Vault
/// credential change outside the transaction that locked the current service.
async fn update_kubernetes_pod(
    api: &Api,
    mut txn: db::Transaction<'_>,
    service_id: ExtensionServiceId,
    current_service: &ExtensionService,
    req: rpc::UpdateDpuExtensionServiceRequest,
) -> Result<(ExtensionService, ExtensionServiceVersionInfo), Status> {
    let latest_version = extension_service::find_version_info(&mut txn, service_id, None).await?;
    txn.commit().await?;

    validate_extension_service_data_size(&req.data)?;
    validate_pod_spec_file(&req.data)?;
    if let Some(credential) = &req.credential {
        validate_extension_service_credential(&current_service.service_type, credential)?;
    }

    let latest_credential = if latest_version.has_credential {
        Some(
            get_extension_service_credential(
                &api.credential_manager,
                create_extension_service_credential_key(&service_id, latest_version.version),
            )
            .await?,
        )
    } else {
        None
    };
    if !detect_kubernetes_pod_service_spec_change(
        &req.data,
        &latest_version.data,
        req.credential.clone(),
        latest_credential,
    )? {
        return Err(CarbideError::InvalidArgument(
            "no changes to data or credential from latest version".to_string(),
        )
        .into());
    }

    let obvs_len = req
        .observability
        .as_ref()
        .map(|o| o.configs.len())
        .unwrap_or(0);
    if obvs_len > MAX_OBSERVABILITY_CONFIG_PER_SERVICE {
        return Err(CarbideError::InvalidConfiguration(
                model::ConfigValidationError::InvalidValue(format!(
                    "{} configured observability configs for extension service exceeds the limit of {MAX_OBSERVABILITY_CONFIG_PER_SERVICE}",
                    obvs_len
                )),
            ).into());
    }
    let observability = req
        .observability
        .map(ExtensionServiceObservability::try_from)
        .transpose()?;

    let version_change =
        ConfigVersion::new(current_service.version_ctr.try_into().map_err(|e| {
            CarbideError::internal(format!("invalid version for extension service: {e}"))
        })?)
        .incremental_change();

    // Store the new credential in Vault if provided. We have to do this before updating the
    // data in the database, so that in case this fails, the database remains untouched. We
    // can't use db transactions for this since it can cause issues if vault is unresponsive.
    //
    // It does mean we have to inherit the service_type from the current service, rather than
    // the updated one, which is ok because that is not being updated here. It also means we
    // have to pick the new version ourselves by incrermenting the current version, but this is
    // safe because the database will use "WHERE version_ctr = {old_version}", failing if there
    // is a race.
    let vault_credential_created = if let Some(credential) = &req.credential {
        create_extension_service_credential(
            &current_service.service_type,
            &api.credential_manager,
            create_extension_service_credential_key(&service_id, version_change.new),
            credential,
        )
        .await?;
        true
    } else {
        false
    };

    match api
        .with_txn(|txn| {
            extension_service::update(
                txn,
                service_id,
                req.service_name.as_deref(),
                req.description.as_deref(),
                &[],
                &req.data,
                observability,
                req.credential.is_some(),
                version_change,
            )
            .boxed()
        })
        .await
    {
        Ok(Ok(result)) => Ok(result),
        Err(error) | Ok(Err(error)) => {
            if vault_credential_created {
                let credential_key =
                    create_extension_service_credential_key(&service_id, version_change.new);
                if let Err(delete_error) =
                    delete_extension_service_credential(&api.credential_manager, credential_key)
                        .await
                {
                    emit(ExtensionServiceCredentialCleanupFailed::Update {
                        extension_service_id: service_id,
                        error: delete_error.to_string(),
                    });
                }
            }
            Err(error.into())
        }
    }
}

async fn updated_extension_service_response(
    api: &Api,
    service_id: ExtensionServiceId,
    updated_service: ExtensionService,
    latest_version: ExtensionServiceVersionInfo,
) -> Result<Response<rpc::DpuExtensionService>, Status> {
    let versions =
        extension_service::find_all_versions(&api.database_connection, service_id).await?;
    let lifecycle_status = ::rpc::model::extension_service::lifecycle_status(
        updated_service.status.controller_state.value,
        updated_service.status.controller_state.version,
        updated_service.status.controller_state_outcome.clone(),
    );

    let response = rpc::DpuExtensionService {
        dpu_target: updated_service
            .dpu_target
            .map(|target| rpc::DpuExtensionServiceDpuTarget::from(target) as i32),
        service_id: service_id.to_string(),
        service_type: rpc::DpuExtensionServiceType::from(updated_service.service_type.clone())
            as i32,
        service_name: updated_service.name,
        tenant_organization_id: updated_service.tenant_organization_id.to_string(),
        version_ctr: updated_service.version_ctr,
        active_versions: versions.iter().map(ToString::to_string).collect(),
        latest_version_info: Some(latest_version.into()),
        description: updated_service.description,
        created: updated_service.created.to_string(),
        updated: updated_service.updated.to_string(),
        lifecycle_status: Some(lifecycle_status),
        service_vpc_interfaces: updated_service
            .service_vpc_interfaces
            .into_iter()
            .map(Into::into)
            .collect(),
    };

    Ok(Response::new(response))
}

/// Deletes an extension service or specific versions.
/// - Soft-deletes all versions if `versions` field is empty, or specific versions if provided
/// - Checks if any versions are in use by instances before deletion
/// - Soft-deletes the service itself if no versions remain
/// - Removes associated credentials for all deleted versions from Vault
pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteDpuExtensionServiceRequest>,
) -> Result<Response<rpc::DeleteDpuExtensionServiceResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let service_id = req.service_id.parse::<ExtensionServiceId>().map_err(|e| {
        CarbideError::from(RpcDataConversionError::InvalidUuid(
            "ExtensionServiceId",
            e.to_string(),
        ))
    })?;

    let mut txn = api.txn_begin().await?;

    // Parse versions from strings to ConfigVersion
    let versions: Vec<config_version::ConfigVersion> = req
        .versions
        .iter()
        .map(|v| {
            v.parse::<config_version::ConfigVersion>().map_err(|e| {
                CarbideError::from(RpcDataConversionError::InvalidConfigVersion(format!(
                    "Failed to parse version: {}",
                    e
                )))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Lock the extension service for delete so that no other request can update the service.
    // Include a soft-deleted DPF Helm service so a retry can acknowledge an
    // already accepted deletion while its external finalization is pending.
    // The Kubernetes Pod deletion path still rejects soft-deleted services.
    let current_service_res =
        extension_service::find_by_ids(&mut txn, &[service_id], true, true).await?;
    match current_service_res.len() {
        0 => {
            return Err(CarbideError::NotFoundError {
                kind: "extension_service",
                id: service_id.to_string(),
            }
            .into());
        }
        1 => {}
        _ => {
            return Err(CarbideError::Internal {
                message: "Multiple extension services found for the same ID".to_string(),
            }
            .into());
        }
    };
    let current_service = &current_service_res[0];

    match &current_service.service_type {
        ExtensionServiceType::DpfHelmChart => {
            delete_dpf_helm_chart(&mut txn, service_id, current_service, &versions).await?;
            txn.commit().await?;
        }
        ExtensionServiceType::KubernetesPod => {
            let credential_versions =
                delete_kubernetes_pod(&mut txn, service_id, current_service, &versions).await?;

            txn.commit().await?;

            // Delete credentials from Vault for the deleted versions that had credentials
            // Note: This happens after the transaction commit, so it's best-effort cleanup
            for version in &credential_versions {
                let credential_key = create_extension_service_credential_key(&service_id, *version);

                // The database deletion is already committed, so credential
                // cleanup stays best effort and does not fail the API request.
                if let Err(error) =
                    delete_extension_service_credential(&api.credential_manager, credential_key)
                        .await
                {
                    emit(ExtensionServiceCredentialCleanupFailed::Delete {
                        extension_service_id: service_id,
                        version: *version,
                        error: error.to_string(),
                    });
                }
            }
        }
    };

    Ok(Response::new(rpc::DeleteDpuExtensionServiceResponse {}))
}

/// Records a DPF Helm chart delete request for asynchronous DPUService cleanup.
/// The service's stable V1 remains reserved while DPF finalizers complete.
async fn delete_dpf_helm_chart(
    txn: &mut db::Transaction<'_>,
    service_id: ExtensionServiceId,
    service: &ExtensionService,
    requested_versions: &[ConfigVersion],
) -> Result<(), Status> {
    if requested_versions.len() > 1
        || requested_versions
            .iter()
            .any(|version| version.version_nr() != 1)
    {
        return Err(CarbideError::InvalidArgument(
            "DPF helm chart extension service deletion accepts only an omitted version or V1"
                .to_string(),
        )
        .into());
    }

    // A delete request is idempotent while DPF finalizers still retain the
    // soft-deleted service row for controller reconciliation.
    if service.deleted.is_some() {
        if service.status.controller_state.value == ExtensionServiceLifecycleState::Deleting {
            return Ok(());
        }
        return Err(CarbideError::NotFoundError {
            kind: "extension_service",
            id: service_id.to_string(),
        }
        .into());
    }

    let controller_state = service.status.controller_state.value;
    let version: ConfigVersion =
        extension_service::find_version_info_of_known_service(txn, service_id, None)
            .await?
            .version;

    if extension_service::is_service_in_use(txn, service_id, &[version], true).await? {
        return Err(CarbideError::FailedPrecondition(
            "extension service is in use by instances; detach before deleting".into(),
        )
        .into());
    }

    extension_service::request_dpf_helm_chart_deletion(
        txn,
        service_id,
        version,
        &controller_state,
        service.status.controller_state.version,
    )
    .await?;

    Ok(())
}

/// Deletes Kubernetes Pod service versions directly from NICo's persistence.
async fn delete_kubernetes_pod(
    txn: &mut db::Transaction<'_>,
    service_id: ExtensionServiceId,
    service: &ExtensionService,
    versions: &[ConfigVersion],
) -> Result<Vec<ConfigVersion>, Status> {
    if service.deleted.is_some() {
        return Err(CarbideError::NotFoundError {
            kind: "extension_service",
            id: service_id.to_string(),
        }
        .into());
    }

    // A soft-deleted, terminating instance counts as in use.
    if extension_service::is_service_in_use(txn, service_id, versions, true).await? {
        return Err(CarbideError::FailedPrecondition(
            "one or more extension service version is in use by instances; detach it, or wait for \
             instance deletion to complete, before deleting"
                .into(),
        )
        .into());
    }

    let credential_versions =
        extension_service::find_versions_with_credentials(txn, service_id, versions).await?;

    let deleted_versions =
        extension_service::soft_delete_versions(txn, service_id, versions).await?;

    if !deleted_versions.is_empty() {
        if extension_service::find_all_versions(&mut *txn, service_id)
            .await?
            .is_empty()
        {
            extension_service::soft_delete_service(
                txn,
                service_id,
                service.status.controller_state.version,
            )
            .await?;
        } else {
            extension_service::set_updated_timestamp(txn, service_id).await?;
        }
    }

    Ok(credential_versions)
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::DpuExtensionServiceSearchFilter>,
) -> Result<Response<rpc::DpuExtensionServiceIdList>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Log tenant organization ID if present
    if let Some(ref tenant_org_id_str) = req.tenant_organization_id {
        log_tenant_organization_id(tenant_org_id_str);
    }

    // Validate tenant organization ID
    let tenant_organization_id: Option<TenantOrganizationId> = req
        .tenant_organization_id
        .as_deref() // avoid moving the String; parse from &str
        .map(|id| {
            id.parse::<TenantOrganizationId>().map_err(|e| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })
        })
        .transpose()?; // Result<Option<TenantOrganizationId>, CarbideError>

    // Convert the service type from the request
    let service_type_opt: Option<ExtensionServiceType> = match req.service_type {
        None => None,
        Some(v) => {
            let service_type_rpc = rpc::DpuExtensionServiceType::try_from(v)
                .map_err(|_| CarbideError::InvalidArgument("invalid service_type".to_string()))?;
            Some(ExtensionServiceType::from(service_type_rpc))
        }
    };

    let mut txn = api.txn_begin().await?;

    let ids = extension_service::find_ids(
        &mut txn,
        service_type_opt,
        req.name.as_deref(),
        tenant_organization_id.as_ref(),
        false,
        false,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(rpc::DpuExtensionServiceIdList {
        service_ids: ids.into_iter().map(|id| id.to_string()).collect(),
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::DpuExtensionServicesByIdsRequest>,
) -> Result<Response<rpc::DpuExtensionServiceList>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Parse the service IDs from the request
    let mut ids: Vec<ExtensionServiceId> = Vec::with_capacity(req.service_ids.len());
    for s in &req.service_ids {
        let id = s.parse::<ExtensionServiceId>().map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidUuid(
                "ExtensionServiceId",
                e.to_string(),
            ))
        })?;
        ids.push(id);
    }

    let mut txn = api.txn_begin().await?;

    let snapshots = extension_service::find_snapshots_by_ids(&mut txn, &ids)
        .await
        .map_err(Status::from)?;

    txn.commit().await?;

    let services_resp = snapshots
        .into_iter()
        .map(|snapshot| snapshot.into())
        .collect();

    Ok(Response::new(rpc::DpuExtensionServiceList {
        services: services_resp,
    }))
}

/// Get the version info based on the service ID and version.
/// If version is not provided, return all version infos of the extension service.
pub(crate) async fn get_versions_info(
    api: &Api,
    request: Request<rpc::GetDpuExtensionServiceVersionsInfoRequest>,
) -> Result<Response<rpc::DpuExtensionServiceVersionInfoList>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Parse versions from strings to ConfigVersions
    let versions: Option<Vec<ConfigVersion>> = req
        .versions
        .iter()
        .map(|v| v.parse::<config_version::ConfigVersion>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidConfigVersion(format!(
                "Failed to parse version: {}",
                e
            )))
        })?
        .none_if_empty();

    let mut txn = api.txn_begin().await?;

    let service_id = req.service_id.parse::<ExtensionServiceId>().map_err(|e| {
        CarbideError::from(RpcDataConversionError::InvalidUuid(
            "ExtensionServiceId",
            e.to_string(),
        ))
    })?;

    // req.versions is optional, if not provided, return all version infos of the extension service
    let versions_opt = versions.as_deref();
    let versions = extension_service::find_versions_info(&mut txn, &service_id, versions_opt)
        .await
        .map_err(Status::from)?;

    txn.commit().await?;

    Ok(Response::new(rpc::DpuExtensionServiceVersionInfoList {
        version_infos: versions.into_iter().map(|version| version.into()).collect(),
    }))
}

/// Find instances that have this extension service configured, or has this extension service
/// terminating.
/// If version is provided in the request, only return instances that have this specific version
/// configured or terminating.
pub(crate) async fn find_instances_by_extension_service(
    api: &Api,
    request: Request<rpc::FindInstancesByDpuExtensionServiceRequest>,
) -> Result<Response<rpc::FindInstancesByDpuExtensionServiceResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Parse and validate extension service ID
    let service_id = req.service_id.parse::<ExtensionServiceId>().map_err(|e| {
        CarbideError::from(RpcDataConversionError::InvalidUuid(
            "ExtensionServiceId",
            e.to_string(),
        ))
    })?;

    // Parse version from string to ConfigVersion if provided
    let version = req
        .version
        .map(|v| {
            v.parse::<config_version::ConfigVersion>().map_err(|e| {
                CarbideError::from(RpcDataConversionError::InvalidConfigVersion(format!(
                    "Failed to parse version: {}",
                    e
                )))
            })
        })
        .transpose()?;

    let mut txn = api.txn_begin().await?;

    // Verify extension service exists
    let extension_service_res =
        extension_service::find_by_ids(&mut txn, &[service_id], false, false).await?;
    match extension_service_res.len() {
        0 => {
            return Err(CarbideError::NotFoundError {
                kind: "extension_service",
                id: service_id.to_string(),
            }
            .into());
        }
        1 => {}
        _ => {
            return Err(CarbideError::Internal {
                message: "Multiple extension services found for the same ID".to_string(),
            }
            .into());
        }
    };

    // Find instances that have this extension service (and optionally a specific version)
    // in its db extension services config
    let instances = instance::find_by_extension_service(&mut txn, service_id, version).await?;
    let mut instance_infos: Vec<rpc::InstanceDpuExtensionServiceInfo> = Vec::new();

    for instance in instances {
        // Get the extension service config for this instance
        if let Some(ext_service_config) = instance
            .config
            .extension_services
            .service_configs
            .iter()
            .find(|config| config.service_id == service_id)
        {
            let service_version = ext_service_config.version;
            instance_infos.push(rpc::InstanceDpuExtensionServiceInfo {
                instance_id: instance.id.to_string(),
                service_id: service_id.to_string(),
                version: service_version.to_string(),
                removed: ext_service_config.removed.map(|ts| ts.to_string()),
            });
        } else {
            return Err(CarbideError::Internal {
                message: format!(
                    "Instance {} returned by database query but no extension service config found",
                    instance.id
                ),
            }
            .into());
        }
    }

    txn.commit().await?;

    Ok(Response::new(
        rpc::FindInstancesByDpuExtensionServiceResponse {
            instances: instance_infos,
        },
    ))
}

// Validate whether the reqested service type is supported at the site.
fn validate_extension_service_type_enabled(
    service_type: &ExtensionServiceType,
    dpf_enabled: bool,
) -> Result<(), CarbideError> {
    // DPF Helm-chart services require DPF to be enabled for the site.
    if matches!(service_type, ExtensionServiceType::DpfHelmChart) && !dpf_enabled {
        return Err(CarbideError::FailedPrecondition(
            "DPF helm chart extension services require DPF to be enabled for this site".to_string(),
        ));
    }

    Ok(())
}

/// Validates the pod spec file format for KubernetesPod service.
/// The pod spec file must be a valid YAML/JSON object that must contain the following fields:
/// - apiVersion
/// - kind
/// - metadata.name
/// - spec.containers (must be an array and must have at least one container)
fn validate_pod_spec_file(data: &str) -> Result<(), CarbideError> {
    if data.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "invalid empty data for KubernetesPod service, need a valid pod manifest".to_string(),
        ));
    }

    let root = serde_yaml::from_str::<serde_yaml::Value>(data).map_err(|e| {
        CarbideError::InvalidArgument(format!(
            "invalid pod spec file for KubernetesPod service: {}",
            e
        ))
    })?;

    match root {
        serde_yaml::Value::Mapping(ref mapping) => {
            // Check for apiVersion field
            if !mapping.contains_key(serde_yaml::Value::String("apiVersion".to_string())) {
                return Err(CarbideError::InvalidArgument(
                    "pod manifest missing required field: apiVersion".to_string(),
                ));
            }

            // Check for kind field and verify it's "Pod"
            let kind = mapping
                .get(serde_yaml::Value::String("kind".to_string()))
                .and_then(|v| v.as_str());
            if kind != Some("Pod") {
                return Err(CarbideError::InvalidArgument(
                    // xtask:allow-error-case: `Pod` is a case-sensitive Kubernetes kind
                    "pod manifest must have kind: Pod".to_string(),
                ));
            }

            // Check for metadata.name field
            let metadata = mapping
                .get(serde_yaml::Value::String("metadata".to_string()))
                .and_then(|v| v.as_mapping());
            match metadata {
                Some(meta_map) => {
                    if !meta_map.contains_key(serde_yaml::Value::String("name".to_string())) {
                        return Err(CarbideError::InvalidArgument(
                            "pod manifest missing required field: metadata.name".to_string(),
                        ));
                    }
                }
                None => {
                    return Err(CarbideError::InvalidArgument(
                        "pod manifest missing required field: metadata".to_string(),
                    ));
                }
            }

            // Check for spec.containers as a non-empty array
            let spec = mapping
                .get(serde_yaml::Value::String("spec".to_string()))
                .and_then(|v| v.as_mapping());
            match spec {
                Some(spec_map) => {
                    let containers = spec_map
                        .get(serde_yaml::Value::String("containers".to_string()))
                        .and_then(|v| v.as_sequence());

                    match containers {
                        Some(container_list) => {
                            if container_list.is_empty() {
                                return Err(CarbideError::InvalidArgument(
                                    "pod manifest must have at least one container in spec.containers".to_string(),
                                ));
                            }
                        }
                        None => {
                            return Err(CarbideError::InvalidArgument(
                                "pod manifest missing required field: spec.containers (must be an array)".to_string(),
                            ));
                        }
                    }
                }
                None => {
                    return Err(CarbideError::InvalidArgument(
                        "pod manifest missing required field: spec".to_string(),
                    ));
                }
            }
        }
        _ => {
            return Err(CarbideError::InvalidArgument(
                "pod manifest must be a valid mapping object that contains apiVersion, kind, metadata, and spec.containers".to_string(),
            ))
        }
    };

    Ok(())
}

/// Parses and validates a DPF Helm chart definition, returning the stable JSON
/// representation stored as the service's desired state.
fn parse_dpf_helm_chart_data(data: &str) -> Result<String, CarbideError> {
    DpfHelmChartServiceData::parse(data)
        .and_then(|definition| definition.normalized_json())
        .map_err(|error| CarbideError::InvalidArgument(error.to_string()))
}

/// Rejects extension-service definitions that exceed the API size limit.
fn validate_extension_service_data_size(data: &str) -> Result<(), CarbideError> {
    if data.len() > MAX_DATA_SIZE {
        return Err(CarbideError::InvalidArgument(format!(
            "extension service data exceeds the maximum size: {} bytes",
            MAX_DATA_SIZE
        )));
    }

    Ok(())
}

/// Validates extension service credential fields based on service type
fn validate_extension_service_credential(
    service_type: &ExtensionServiceType,
    credential: &rpc::DpuExtensionServiceCredential,
) -> Result<(), CarbideError> {
    match service_type {
        ExtensionServiceType::KubernetesPod => {
            // Validate registry URL, this will be fed into the credential provider as
            // image match pattern. For example, if the registry URL is "nvcr.io/nvforge",
            // kubelet will match all images under "nvcr.io/nvforge/*".
            if credential.registry_url.is_empty() || credential.registry_url.len() > 255 {
                return Err(CarbideError::InvalidArgument(
                    "invalid credential registry URL".to_string(),
                ));
            }
        }

        // DPF Helm credentials need a DPF-native secret/ownership contract;
        // do not send the legacy DPU-agent credential representation to DPF.
        ExtensionServiceType::DpfHelmChart => {
            return Err(CarbideError::FailedPrecondition(
                "credentials for DPF helm chart extension services should be preprovisioned and are not supported through API"
                    .to_string(),
            ));
        }
    }

    match credential.r#type.as_ref() {
        Some(rpc::dpu_extension_service_credential::Type::UsernamePassword(up)) => {
            // @TODO(Felicity): Add more validation for username and password
            if up.username.is_empty() || up.username.len() > 255 {
                return Err(CarbideError::InvalidArgument(
                    "invalid username".to_string(),
                ));
            }
            if up.password.is_empty() || up.password.len() > 255 {
                return Err(CarbideError::InvalidArgument(
                    "invalid password".to_string(),
                ));
            }
        }
        _ => {
            return Err(CarbideError::InvalidArgument(
                "invalid credential type".to_string(),
            ));
        }
    };

    Ok(())
}

/// Return true/false based on if there are any changes between old and new extension service specifications.
fn detect_kubernetes_pod_service_spec_change(
    new_data: &str,
    old_data: &str,
    new_cred: Option<rpc::DpuExtensionServiceCredential>,
    old_cred: Option<rpc::DpuExtensionServiceCredential>,
) -> Result<bool, CarbideError> {
    let old_data_yaml = serde_yaml::from_str::<serde_yaml::Value>(old_data).map_err(|e| {
        CarbideError::internal(format!(
            "found corrupted data for KubernetesPod service: {}",
            e
        ))
    })?;
    let new_data_yaml = serde_yaml::from_str::<serde_yaml::Value>(new_data).map_err(|e| {
        CarbideError::InvalidArgument(format!(
            "invalid pod spec file for KubernetesPod service: {}",
            e
        ))
    })?;
    let data_changed = old_data_yaml != new_data_yaml;

    let cred_changed = match (old_cred.as_ref(), new_cred.as_ref()) {
        (None, None) => false,
        (Some(a), Some(b)) => a != b,
        _ => true,
    };

    Ok(data_changed || cred_changed)
}

/// Create a credential key for extension service registry credentials
pub(super) fn create_extension_service_credential_key(
    service_id: &ExtensionServiceId,
    version: ConfigVersion,
) -> CredentialKey {
    CredentialKey::ExtensionService {
        service_id: service_id.to_string(),
        version: version.version_nr().to_string(),
    }
}

/// Create the extension service credential in the vault based on the credential type
async fn create_extension_service_credential(
    service_type: &ExtensionServiceType,
    credential_writer: &dyn carbide_secrets::credentials::CredentialWriter,
    credential_key: CredentialKey,
    credential: &rpc::DpuExtensionServiceCredential,
) -> Result<(), CarbideError> {
    match service_type {
        ExtensionServiceType::KubernetesPod => {
            use ::rpc::forge::dpu_extension_service_credential::Type as CredType;

            match credential.r#type.as_ref() {
                Some(CredType::UsernamePassword(up)) => {
                    // The username format is "url: {registry_url}, username: {username}" for KubernetesPod service credentials
                    // Because we don't have a separate field for registry_url in the credential struct in vault
                    let cred_username = format!(
                        "url: {}, username: {}",
                        credential.registry_url, up.username
                    );

                    let cred = carbide_secrets::credentials::Credentials::UsernamePassword {
                        username: cred_username,
                        password: up.password.clone(),
                    };

                    credential_writer
                        .create_credentials(&credential_key, &cred)
                        .await
                        .map_err(|e| {
                            CarbideError::internal(format!(
                                "error creating credential for extension service: {e}"
                            ))
                        })
                }
                None => Err(CarbideError::InvalidArgument(
                    "missing credential".to_string(),
                )),
            }
        }
        // This is normally rejected by validation before reaching the Vault.
        // Keep the same boundary here so a future caller cannot accidentally
        // store a DPF Helm credential in the legacy DPU-agent format.
        ExtensionServiceType::DpfHelmChart => Err(CarbideError::FailedPrecondition(
            "credentials for DPF helm chart extension services are not supported yet".to_string(),
        )),
    }
}

/// Delete the extension service credential from the vault using the credential key
async fn delete_extension_service_credential(
    credential_writer: &dyn carbide_secrets::credentials::CredentialWriter,
    credential_key: CredentialKey,
) -> Result<(), eyre::Report> {
    credential_writer
        .delete_credentials(&credential_key)
        .await?;
    Ok(())
}

/// Get the extension service credential from the vault using the credential key
pub(super) async fn get_extension_service_credential(
    credential_reader: &dyn carbide_secrets::credentials::CredentialReader,
    credential_key: CredentialKey,
) -> Result<rpc::DpuExtensionServiceCredential, CarbideError> {
    let credential = credential_reader
        .get_credentials(&credential_key)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Could not find the credential: {}", e),
        })?;

    let (registry_url, username, password) = match credential {
        Some(Credentials::UsernamePassword { username, password }) => {
            // The username format is "url: {registry_url}, username: {username}" for KubernetesPod service credentials
            // Because we store the credential in the vault as a single string, we need to parse it to get the registry URL and username
            let parts: Vec<&str> = username.splitn(2, ", username: ").collect();

            if parts.len() != 2 {
                return Err(CarbideError::Internal {
                    message: format!("Invalid credential format: {}", username),
                });
            }

            // Extract registry URL (remove "url: " prefix)
            let registry_url = parts[0]
                .strip_prefix("url: ")
                .ok_or_else(|| CarbideError::Internal {
                    message: format!(
                        "Invalid credential format, missing 'url: ' prefix: {}",
                        username
                    ),
                })?
                .to_string();

            let actual_username = parts[1].to_string();

            (registry_url, actual_username, password)
        }
        _ => {
            return Err(CarbideError::Internal {
                message: "Could not find the credential".to_string(),
            });
        }
    };

    Ok(rpc::DpuExtensionServiceCredential {
        registry_url,
        r#type: Some(
            rpc::dpu_extension_service_credential::Type::UsernamePassword(rpc::UsernamePassword {
                username,
                password,
            }),
        ),
    })
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;

    use super::*;

    const CLEANUP_FAILURE_METRIC: &str =
        "carbide_extension_service_credential_cleanup_failures_total";
    const EXTENSION_SERVICE_ID: &str = "00000000-0000-0000-0000-000000000000";
    const VERSION: &str = "V3-T0";

    #[derive(Debug)]
    enum CleanupFailureCase {
        Create,
        Update,
        Delete,
    }

    #[derive(Debug, PartialEq)]
    struct CleanupFailureObservation {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        operation: Option<String>,
        extension_service_id: Option<String>,
        version: Option<String>,
        error: Option<String>,
        counter_delta: f64,
    }

    #[test]
    fn credential_cleanup_failures_log_and_count_by_operation() {
        value_scenarios!(
            run = |case| {
                let extension_service_id = ExtensionServiceId::nil();
                let version = VERSION.parse::<ConfigVersion>().unwrap();
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| match case {
                    CleanupFailureCase::Create => {
                        emit(ExtensionServiceCredentialCleanupFailed::Create {
                            extension_service_id,
                            error: "credential delete failed".to_string(),
                        });
                    }
                    CleanupFailureCase::Update => {
                        emit(ExtensionServiceCredentialCleanupFailed::Update {
                            extension_service_id,
                            error: "credential delete failed".to_string(),
                        });
                    }
                    CleanupFailureCase::Delete => {
                        emit(ExtensionServiceCredentialCleanupFailed::Delete {
                            extension_service_id,
                            version,
                            error: "credential delete failed".to_string(),
                        });
                    }
                });
                assert_eq!(logs.len(), 1, "each cleanup failure should write one record");
                let log = logs.first().expect("cleanup failure Event did not log");
                let operation = log.field("operation").map(str::to_string);

                CleanupFailureObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    operation: operation.clone(),
                    extension_service_id: log
                        .field("extension_service_id")
                        .map(str::to_string),
                    version: log.field("version").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    counter_delta: metrics.counter_delta(
                        CLEANUP_FAILURE_METRIC,
                        &[("operation", operation.as_deref().unwrap())],
                    ),
                }
            };
            "create transaction cleanup fails" {
                CleanupFailureCase::Create => CleanupFailureObservation {
                    level: tracing::Level::WARN,
                    metadata_name: "extension_service_credential_cleanup_failed".to_string(),
                    message: "Failed to delete extension service credential after transaction failure".to_string(),
                    event_name: Some("extension_service_credential_cleanup_failed".to_string()),
                    metric_name: Some(CLEANUP_FAILURE_METRIC.to_string()),
                    operation: Some("create".to_string()),
                    extension_service_id: Some(EXTENSION_SERVICE_ID.to_string()),
                    version: None,
                    error: Some("credential delete failed".to_string()),
                    counter_delta: 1.0,
                },
            }
            "update transaction cleanup fails" {
                CleanupFailureCase::Update => CleanupFailureObservation {
                    level: tracing::Level::WARN,
                    metadata_name: "extension_service_credential_cleanup_failed".to_string(),
                    message: "Failed to delete extension service credential after transaction failure".to_string(),
                    event_name: Some("extension_service_credential_cleanup_failed".to_string()),
                    metric_name: Some(CLEANUP_FAILURE_METRIC.to_string()),
                    operation: Some("update".to_string()),
                    extension_service_id: Some(EXTENSION_SERVICE_ID.to_string()),
                    version: None,
                    error: Some("credential delete failed".to_string()),
                    counter_delta: 1.0,
                },
            }
            "post-commit delete cleanup fails" {
                CleanupFailureCase::Delete => CleanupFailureObservation {
                    level: tracing::Level::WARN,
                    metadata_name: "extension_service_credential_cleanup_failed".to_string(),
                    message: "Failed to delete extension service credential".to_string(),
                    event_name: Some("extension_service_credential_cleanup_failed".to_string()),
                    metric_name: Some(CLEANUP_FAILURE_METRIC.to_string()),
                    operation: Some("delete".to_string()),
                    extension_service_id: Some(EXTENSION_SERVICE_ID.to_string()),
                    version: Some(VERSION.to_string()),
                    error: Some("credential delete failed".to_string()),
                    counter_delta: 1.0,
                },
            }
        );
    }
}
