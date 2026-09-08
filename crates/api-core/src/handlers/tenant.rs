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
use model::ConfigValidationError;
use model::metadata::Metadata;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_tenant_organization_id};

/// Ensures that fields unsupported by the tenant DB model are rejected early.
fn metadata_to_valid_tenant_metadata(metadata: Option<rpc::Metadata>) -> Result<Metadata, Status> {
    Ok(match metadata {
        None => return Err(CarbideError::MissingArgument("metadata").into()),
        Some(mdata) => {
            if !mdata.description.is_empty() {
                return Err(CarbideError::InvalidConfiguration(
                    ConfigValidationError::InvalidValue(
                        "description not supported for tenant metadata".into(),
                    ),
                )
                .into());
            }

            if !mdata.labels.is_empty() {
                return Err(CarbideError::InvalidConfiguration(
                    ConfigValidationError::InvalidValue(
                        "labels not supported for tenant metadata".into(),
                    ),
                )
                .into());
            }

            mdata.try_into().map_err(CarbideError::from)?
        }
    })
}

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateTenantRequest>,
) -> Result<Response<rpc::CreateTenantResponse>, Status> {
    crate::api::log_request_data(&request);

    let rpc::CreateTenantRequest {
        organization_id,
        metadata,
        routing_profile_type,
    } = request.into_inner();

    log_tenant_organization_id(&organization_id);

    let metadata: Metadata = metadata_to_valid_tenant_metadata(metadata)?;

    metadata.validate(true).map_err(CarbideError::from)?;

    let routing_profile_type = match (api.runtime_config.fnn.is_some(), routing_profile_type) {
        (fnn_enabled, Some(profile)) => {
            // If a profile name was sent in, verify that it matches
            // a defined profile.
            //  If FNN isn't enabled, the caller shouldn't be sending the value.
            if !fnn_enabled
                || !api
                    .runtime_config
                    .fnn
                    .as_ref()
                    .map(|c| c.routing_profiles.contains_key(&profile))
                    .unwrap_or_default()
            {
                return Err(CarbideError::NotFoundError {
                    kind: "RoutingProfile",
                    id: profile,
                }
                .into());
            }

            Some(profile)
        }
        // If FNN is enabled, and the caller did not send the value, we'll default.
        (true, None) => Some(
            api.runtime_config
                .default_tenant_routing_profile_type
                .clone(),
        ),
        // If FNN isn't enabled, and nothing was sent, nothing to do.
        (false, None) => None,
    };

    let mut txn = api.txn_begin().await?;

    let response =
        db::tenant::create_and_persist(organization_id, metadata, routing_profile_type, &mut txn)
            .await?
            .try_into()
            .map(Response::new)
            .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(response)
}

pub(crate) async fn find(
    api: &Api,
    request: Request<rpc::FindTenantRequest>,
) -> Result<Response<rpc::FindTenantResponse>, Status> {
    crate::api::log_request_data(&request);

    let rpc::FindTenantRequest {
        tenant_organization_id,
    } = request.into_inner();

    log_tenant_organization_id(&tenant_organization_id);

    let mut txn = api.txn_begin().await?;

    let response = match db::tenant::find(tenant_organization_id, false, &mut txn)
        .await
        .map(Response::new)?
        .into_inner()
    {
        None => rpc::FindTenantResponse {
            tenant: None,
            permitted_routing_profile_types: vec![],
        },
        Some(t) => {
            let permitted_routing_profile_types = match (
                t.routing_profile_type.as_deref(),
                api.runtime_config.fnn.as_ref(),
            ) {
                (Some(tenant_profile_type), Some(fnn)) => {
                    match fnn.routing_profiles.get(tenant_profile_type) {
                        Some(tenant_profile) => {
                            let tenant_access_tier = tenant_profile.access_tier.unwrap_or_default();
                            let mut permitted = fnn
                                .routing_profiles
                                .iter()
                                .filter(|(_, profile)| {
                                    profile.access_tier.unwrap_or_default() >= tenant_access_tier
                                })
                                .map(|(name, _)| name.clone())
                                .collect::<Vec<_>>();
                            permitted.sort();
                            permitted
                        }
                        None => {
                            tracing::warn!(
                                organization_id = %t.organization_id,
                                %tenant_profile_type,
                                "tenant routing profile is not present in the current FNN config"
                            );
                            vec![]
                        }
                    }
                }
                _ => vec![],
            };
            let mut response: rpc::FindTenantResponse = t.try_into().map_err(CarbideError::from)?;
            response.permitted_routing_profile_types = permitted_routing_profile_types;
            response
        }
    };

    txn.commit().await?;

    Ok(Response::new(response))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateTenantRequest>,
) -> Result<Response<rpc::UpdateTenantResponse>, Status> {
    crate::api::log_request_data(&request);

    let rpc::UpdateTenantRequest {
        organization_id,
        if_version_match,
        metadata,
        routing_profile_type,
    } = request.into_inner();

    log_tenant_organization_id(&organization_id);

    let metadata: Metadata = metadata_to_valid_tenant_metadata(metadata)?;

    metadata.validate(true).map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    // Grab the tenant details and a row-lock
    let Some(current_tenant) = db::tenant::find(&organization_id, true, &mut txn).await? else {
        return Err(CarbideError::NotFoundError {
            kind: "tenant",
            id: organization_id.clone(),
        }
        .into());
    };

    // We won't use it if FNN isn't enabled, but we can still map so a caller integrating
    // with us before FNN is enabled on a site will be told if they're sending invalid values.
    if let Some(profile) = routing_profile_type.as_ref() {
        // If a profile name was sent in, verify that it matches
        // a defined profile.
        if !api
            .runtime_config
            .fnn
            .as_ref()
            .map(|c| c.routing_profiles.contains_key(profile))
            .unwrap_or_default()
        {
            return Err(CarbideError::NotFoundError {
                kind: "RoutingProfile",
                id: profile.to_owned(),
            }
            .into());
        }
    };

    // If a tenant routing profile is being updated,
    // it can only be allowed if there are no existing VPCs
    // for the tenant.  Technically, at the moment, it's probably
    // ok to allow it as long it's not switching between profiles
    // that have a differing `internal` value (e.g., switching from
    // internal==true to false), but total restriction is safer
    // and easy to loosen later if we find we need it.
    if current_tenant.routing_profile_type != routing_profile_type
        && !db::vpc::find_ids(
            &mut txn,
            model::vpc::VpcSearchFilter {
                tenant_org_id: Some(organization_id.clone()),
                ..Default::default()
            },
        )
        .await?
        .is_empty()
    {
        return Err(CarbideError::FailedPrecondition(
            "cannot update tenant routing profile type for tenant with active VPCs".to_string(),
        )
        .into());
    }

    let expected_version = if let Some(config_version_str) = if_version_match {
        config_version_str.parse().map_err(CarbideError::from)?
    } else {
        current_tenant.version
    };

    let response = db::tenant::update(
        organization_id,
        metadata,
        expected_version,
        routing_profile_type,
        &mut txn,
    )
    .await?
    .try_into()
    .map(Response::new)
    .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(response)
}

pub(crate) async fn find_tenants_by_organization_ids(
    api: &Api,
    request: Request<rpc::TenantByOrganizationIdsRequest>,
) -> Result<Response<rpc::TenantList>, Status> {
    crate::api::log_request_data(&request);
    let request = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let tenant_organization_ids: Vec<String> = request.organization_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if tenant_organization_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if tenant_organization_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let tenants: Vec<rpc::Tenant> =
        db::tenant::load_by_organization_ids(&mut txn, &tenant_organization_ids)
            .await?
            .into_iter()
            .filter_map(|tenant| rpc::Tenant::try_from(tenant).ok())
            .collect();

    txn.commit().await?;

    Ok(tonic::Response::new(rpc::TenantList { tenants }))
}

pub(crate) async fn find_tenant_organization_ids(
    api: &Api,
    request: Request<rpc::TenantSearchFilter>,
) -> Result<Response<rpc::TenantOrganizationIdList>, Status> {
    crate::api::log_request_data(&request);
    let search_config: model::tenant::TenantSearchFilter = request.into_inner().into();
    let tenant_org_ids =
        db::tenant::find_tenant_organization_ids(&api.database_connection, search_config).await?;
    Ok(tonic::Response::new(rpc::TenantOrganizationIdList {
        tenant_organization_ids: tenant_org_ids.into_iter().collect(),
    }))
}

#[cfg(test)]
mod tests {
    use ::rpc::forge as rpc;
    use tonic::Code;

    use super::*;

    #[test]
    fn test_metadata_to_valid_tenant_metadata() {
        // Good metadata
        let metadata = metadata_to_valid_tenant_metadata(Some(rpc::Metadata {
            name: "Name".to_string(),
            description: "".to_string(),
            labels: vec![],
        }));

        assert!(metadata.is_ok());

        // No description allowed
        let metadata = metadata_to_valid_tenant_metadata(Some(rpc::Metadata {
            name: "Name".to_string(),
            description: "should not be stored".to_string(),
            labels: vec![],
        }))
        .unwrap_err();

        assert_eq!(metadata.code(), Code::InvalidArgument);
        assert!(metadata.message().contains("description"));

        // No labels allowed
        let metadata = metadata_to_valid_tenant_metadata(Some(rpc::Metadata {
            name: "Name".to_string(),
            description: "".to_string(),
            labels: vec![rpc::Label {
                key: "aaa".to_string(),
                value: Some("bbb".to_string()),
            }],
        }))
        .unwrap_err();

        assert_eq!(metadata.code(), Code::InvalidArgument);
        assert!(metadata.message().contains("labels"));
    }
}
