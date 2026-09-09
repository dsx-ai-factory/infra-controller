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
use carbide_authn::middleware::Principal;
use carbide_instrument::{Event, LabelValue, emit};
use config_version::ConfigVersion;
use model::attestation::profile::{
    AttestationPolicyDocument, AttestationProfile, DeleteAttestationProfile, NewAttestationProfile,
    UpdateAttestationProfile,
};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::auth::AuthContext;

const KIND: &str = "attestation profile";

#[derive(Clone, Copy, Debug, Eq, LabelValue, PartialEq)]
enum AttestationProfileOperation {
    Created,
    Updated,
    Deleted,
}

/// A profile is security policy, so every accepted change to one is recorded
/// with the version it moved from and to.
#[derive(Event)]
#[event(
    event_name = "attestation_profile_changed",
    metric_name = "carbide_attestation_profile_changes_total",
    component = "nico-api",
    log = info,
    metric = counter,
    message = "Attestation profile changed",
    describe = "Number of accepted attestation profile create, update, and delete operations, by operation."
)]
struct AttestationProfileChanged {
    #[label]
    operation: AttestationProfileOperation,
    #[context]
    hardware_class: String,
    /// Absent on create.
    #[context]
    from_version: Option<String>,
    /// Absent on delete.
    #[context]
    to_version: Option<String>,
    #[context]
    updated_by: String,
    /// The newly stored document; absent on delete.
    #[context]
    policy_document: Option<String>,
}

/// The identity recorded against a profile edit.
///
/// Authentication appends `TrustedCertificate` alongside whichever principal
/// the credential minted, so the first principal that is not that marker is the
/// caller. A chain that minted nothing but was itself trusted leaves the marker
/// as the only honest answer.
fn updated_by(auth_context: Option<&AuthContext>) -> String {
    auth_context
        .and_then(|context| {
            context
                .principals
                .iter()
                .find(|principal| !matches!(principal, Principal::TrustedCertificate))
                .or_else(|| context.principals.first())
        })
        .unwrap_or(&Principal::Anonymous)
        .audit_identity()
}

fn emit_change(
    operation: AttestationProfileOperation,
    hardware_class: &str,
    from_version: Option<ConfigVersion>,
    to_version: Option<ConfigVersion>,
    updated_by: &str,
    policy_document: Option<&AttestationPolicyDocument>,
) {
    emit(AttestationProfileChanged {
        operation,
        hardware_class: hardware_class.to_string(),
        from_version: from_version.map(|version| version.to_string()),
        to_version: to_version.map(|version| version.to_string()),
        updated_by: updated_by.to_string(),
        policy_document: policy_document.and_then(|document| serde_json::to_string(document).ok()),
    })
}

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateAttestationProfileRequest>,
) -> Result<Response<rpc::AttestationProfile>, Status> {
    log_request_data(&request);

    let updated_by = updated_by(request.extensions().get::<AuthContext>());
    let new_profile = NewAttestationProfile::try_from(request.into_inner())?;

    let mut txn = api.txn_begin().await?;
    let profile = db::attestation_profile::create(
        &mut txn,
        &new_profile.hardware_class,
        &new_profile.policy_document,
        &updated_by,
    )
    .await
    // A duplicate class is `already exists` at the API boundary; the database
    // error alone would reach the caller as a failed precondition.
    .map_err(|error| match error {
        db::DatabaseError::AlreadyFoundError { kind, id } => {
            CarbideError::AlreadyFoundError { kind, id }
        }
        error => CarbideError::from(error),
    })?;
    txn.commit().await?;

    emit_change(
        AttestationProfileOperation::Created,
        &profile.hardware_class,
        None,
        Some(profile.version),
        &profile.updated_by,
        Some(&profile.policy_document),
    );

    Ok(Response::new(profile.into()))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateAttestationProfileRequest>,
) -> Result<Response<rpc::AttestationProfile>, Status> {
    log_request_data(&request);

    let updated_by = updated_by(request.extensions().get::<AuthContext>());
    let update = UpdateAttestationProfile::try_from(request.into_inner())?;

    let mut txn = api.txn_begin().await?;
    let current = find_for_update(&mut txn, &update.hardware_class).await?;
    // An omitted `if_version_match` means "whatever is stored now", so the
    // compare-and-swap below stays a single code path.
    let expected_version = update.if_version_match.unwrap_or(current.version);

    let profile = db::attestation_profile::update(
        &mut txn,
        &update.hardware_class,
        &update.policy_document,
        &updated_by,
        expected_version,
    )
    .await?;
    txn.commit().await?;

    emit_change(
        AttestationProfileOperation::Updated,
        &profile.hardware_class,
        Some(current.version),
        Some(profile.version),
        &profile.updated_by,
        Some(&profile.policy_document),
    );

    Ok(Response::new(profile.into()))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteAttestationProfileRequest>,
) -> Result<Response<rpc::DeleteAttestationProfileResponse>, Status> {
    log_request_data(&request);

    let updated_by = updated_by(request.extensions().get::<AuthContext>());
    let delete = DeleteAttestationProfile::try_from(request.into_inner())?;

    let mut txn = api.txn_begin().await?;
    let current = find_for_update(&mut txn, &delete.hardware_class).await?;
    let expected_version = delete.if_version_match.unwrap_or(current.version);

    db::attestation_profile::delete(&mut txn, &delete.hardware_class, expected_version).await?;
    txn.commit().await?;

    emit_change(
        AttestationProfileOperation::Deleted,
        &delete.hardware_class,
        Some(current.version),
        None,
        &updated_by,
        None,
    );

    Ok(Response::new(rpc::DeleteAttestationProfileResponse {}))
}

pub(crate) async fn get(
    api: &Api,
    request: Request<rpc::GetAttestationProfileRequest>,
) -> Result<Response<rpc::AttestationProfile>, Status> {
    log_request_data(&request);

    let hardware_class = request.into_inner().hardware_class;
    let profile = db::attestation_profile::find(&api.database_connection, &hardware_class)
        .await?
        .ok_or_else(|| not_found(&hardware_class))?;

    Ok(Response::new(profile.into()))
}

pub(crate) async fn list(
    api: &Api,
) -> Result<Response<rpc::ListAttestationProfilesResponse>, Status> {
    let profiles = db::attestation_profile::list(&api.database_connection).await?;

    Ok(Response::new(rpc::ListAttestationProfilesResponse {
        profiles: profiles.into_iter().map(Into::into).collect(),
    }))
}

async fn find_for_update(
    txn: &mut sqlx::PgConnection,
    hardware_class: &str,
) -> Result<AttestationProfile, CarbideError> {
    db::attestation_profile::find_for_update(txn, hardware_class)
        .await?
        .ok_or_else(|| not_found(hardware_class))
}

fn not_found(hardware_class: &str) -> CarbideError {
    CarbideError::NotFoundError {
        kind: KIND,
        id: hardware_class.to_string(),
    }
}
