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

use std::fmt::{Display, Formatter, Result as FmtResult};

use ::rpc::forge as rpc;
use itertools::Itertools;
use model::tenant::{
    PublicKey, TenantKeyset, TenantKeysetIdentifier, TenantPublicKey,
    TenantPublicKeyValidationRequest, UpdateTenantKeyset,
};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateTenantKeysetRequest>,
) -> Result<Response<rpc::CreateTenantKeysetResponse>, Status> {
    crate::api::log_request_data(&request);

    let keyset_request: TenantKeyset = request
        .into_inner()
        .try_into()
        .map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    let keyset = db::tenant_keyset::create(&keyset_request, &mut txn).await?;

    txn.commit().await?;

    let public_keys = &keyset_request.keyset_content.public_keys;
    tracing::info!(
        organization_id = keyset_request.keyset_identifier.organization_id.to_string(),
        keyset_id = keyset_request.keyset_identifier.keyset_id,
        public_key_suffixes = PublicKeySuffixes(public_keys).to_string(),
        public_key_suffix_count = public_keys.len(),
        version = keyset_request.version,
        "Tenant keyset created"
    );

    Ok(Response::new(rpc::CreateTenantKeysetResponse {
        keyset: Some(keyset.into()),
    }))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::TenantKeysetSearchFilter>,
) -> Result<Response<rpc::TenantKeysetIdList>, Status> {
    log_request_data(&request);

    let filter: model::tenant::TenantKeysetSearchFilter = request.into_inner().into();

    let keyset_ids = db::tenant_keyset::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::TenantKeysetIdList {
        keyset_ids: keyset_ids
            .into_iter()
            .map(rpc::TenantKeysetIdentifier::from)
            .collect(),
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::TenantKeysetsByIdsRequest>,
) -> Result<Response<rpc::TenantKeySetList>, Status> {
    log_request_data(&request);

    let rpc::TenantKeysetsByIdsRequest {
        keyset_ids,
        include_key_data,
        ..
    } = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if keyset_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if keyset_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let keyset_ids: Vec<model::tenant::TenantKeysetIdentifier> = keyset_ids
        .into_iter()
        .map(|id| id.try_into())
        .collect::<Result<_, _>>()?;

    let keysets =
        db::tenant_keyset::find_by_ids(&api.database_connection, keyset_ids, include_key_data)
            .await;

    let result = keysets
        .map(|vpc| rpc::TenantKeySetList {
            keyset: vpc.into_iter().map(rpc::TenantKeyset::from).collect(),
        })
        .map(Response::new)?;

    Ok(result)
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateTenantKeysetRequest>,
) -> Result<Response<rpc::UpdateTenantKeysetResponse>, Status> {
    crate::api::log_request_data(&request);

    let update_request: UpdateTenantKeyset = request
        .into_inner()
        .try_into()
        .map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    db::tenant_keyset::update(&update_request, &mut txn).await?;

    txn.commit().await?;

    let public_keys = &update_request.keyset_content.public_keys;
    tracing::info!(
        organization_id = update_request.keyset_identifier.organization_id.to_string(),
        keyset_id = update_request.keyset_identifier.keyset_id,
        public_key_suffixes = PublicKeySuffixes(public_keys).to_string(),
        public_key_suffix_count = public_keys.len(),
        version = update_request.version,
        "Tenant keyset updated"
    );

    Ok(Response::new(rpc::UpdateTenantKeysetResponse {}))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteTenantKeysetRequest>,
) -> Result<Response<rpc::DeleteTenantKeysetResponse>, Status> {
    crate::api::log_request_data(&request);

    let rpc::DeleteTenantKeysetRequest { keyset_identifier } = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let Some(keyset_identifier) = keyset_identifier else {
        return Err(CarbideError::MissingArgument("keyset_identifier").into());
    };

    let keyset_identifier: TenantKeysetIdentifier =
        keyset_identifier.try_into().map_err(CarbideError::from)?;

    if !db::tenant_keyset::delete(&keyset_identifier, &mut txn).await? {
        return Err(CarbideError::NotFoundError {
            kind: "keyset",
            id: format!("{keyset_identifier:?}"),
        }
        .into());
    }

    txn.commit().await?;
    tracing::info!(
        organization_id = keyset_identifier.organization_id.to_string(),
        keyset_id = keyset_identifier.keyset_id,
        "Tenant keyset deleted"
    );

    Ok(Response::new(rpc::DeleteTenantKeysetResponse {}))
}

pub(crate) async fn validate_public_key(
    api: &Api,
    request: Request<rpc::ValidateTenantPublicKeyRequest>,
) -> Result<Response<rpc::ValidateTenantPublicKeyResponse>, Status> {
    let request = TenantPublicKeyValidationRequest::try_from(request.into_inner())
        .map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    db::tenant::validate_public_key(&request, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::ValidateTenantPublicKeyResponse {}))
}

struct PublicKeySuffixes<'a>(&'a Vec<TenantPublicKey>);

impl Display for PublicKeySuffixes<'_> {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        self.0
            .iter()
            .map(|v| PublicKeySuffix(&v.public_key))
            .join(",")
            .fmt(fmt)
    }
}

struct PublicKeySuffix<'a>(&'a PublicKey);

impl Display for PublicKeySuffix<'_> {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        const PUB_KEY_SUFFIX_LEN: usize = 8;
        self.0
            .key
            .chars()
            .rev()
            .take(PUB_KEY_SUFFIX_LEN)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>()
            .fmt(fmt)
    }
}
