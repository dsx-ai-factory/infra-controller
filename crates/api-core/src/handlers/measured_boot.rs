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

use ::rpc::protos::measured_boot as pb;
use ::rpc::{forge as rpc_forge, machine_discovery as rpc_md};
use carbide_uuid::machine::MachineId;
use db::attestation::secret_ak_pub;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::api::Api;
use crate::measured_boot::rpc::{bundle, journal, machine, profile, report, site};
use crate::{CarbideError, attestation as attest};

pub(super) async fn create_attest_key_bind_challenge(
    txn: &mut PgConnection,
    attest_key_info: &rpc_md::AttestKeyInfo,
    machine_id: &MachineId,
) -> Result<rpc_forge::AttestKeyBindChallenge, Status> {
    let (matched, ek_pub_rsa) = attest::measured_boot::compare_pub_key_against_cert(
        txn,
        machine_id,
        attest_key_info.ek_pub.as_ref(),
    )
    .await?;
    if !matched {
        return Err(CarbideError::AttestBindKeyError(
            "certificate's public key did not match EK pub key".to_string(),
        )
        .into());
    }

    // generate a secret/credential
    let secret_bytes: [u8; 32] = rand::random();

    let (cli_cred_blob, cli_secret) =
        attest::measured_boot::cli_make_cred(ek_pub_rsa, &attest_key_info.ak_name, &secret_bytes)?;

    secret_ak_pub::insert(txn, &Vec::from(secret_bytes), &attest_key_info.ak_pub).await?;

    Ok(rpc_forge::AttestKeyBindChallenge {
        cred_blob: cli_cred_blob,
        encrypted_secret: cli_secret,
    })
}

pub(crate) async fn create_system_profile(
    api: &Api,
    request: Request<pb::CreateMeasurementSystemProfileRequest>,
) -> Result<Response<pb::CreateMeasurementSystemProfileResponse>, Status> {
    profile::handle_create_system_measurement_profile(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn delete_system_profile(
    api: &Api,
    request: Request<pb::DeleteMeasurementSystemProfileRequest>,
) -> Result<Response<pb::DeleteMeasurementSystemProfileResponse>, Status> {
    profile::handle_delete_measurement_system_profile(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn rename_system_profile(
    api: &Api,
    request: Request<pb::RenameMeasurementSystemProfileRequest>,
) -> Result<Response<pb::RenameMeasurementSystemProfileResponse>, Status> {
    profile::handle_rename_measurement_system_profile(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_system_profile(
    api: &Api,
    request: Request<pb::ShowMeasurementSystemProfileRequest>,
) -> Result<Response<pb::ShowMeasurementSystemProfileResponse>, Status> {
    profile::handle_show_measurement_system_profile(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_system_profiles(
    api: &Api,
    request: Request<pb::ShowMeasurementSystemProfilesRequest>,
) -> Result<Response<pb::ShowMeasurementSystemProfilesResponse>, Status> {
    profile::handle_show_measurement_system_profiles(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_system_profiles(
    api: &Api,
    request: Request<pb::ListMeasurementSystemProfilesRequest>,
) -> Result<Response<pb::ListMeasurementSystemProfilesResponse>, Status> {
    profile::handle_list_measurement_system_profiles(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_system_profile_bundles(
    api: &Api,
    request: Request<pb::ListMeasurementSystemProfileBundlesRequest>,
) -> Result<Response<pb::ListMeasurementSystemProfileBundlesResponse>, Status> {
    profile::handle_list_measurement_system_profile_bundles(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_system_profile_machines(
    api: &Api,
    request: Request<pb::ListMeasurementSystemProfileMachinesRequest>,
) -> Result<Response<pb::ListMeasurementSystemProfileMachinesResponse>, Status> {
    profile::handle_list_measurement_system_profile_machines(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn create_report(
    api: &Api,
    request: Request<pb::CreateMeasurementReportRequest>,
) -> Result<Response<pb::CreateMeasurementReportResponse>, Status> {
    report::handle_create_measurement_report(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn delete_report(
    api: &Api,
    request: Request<pb::DeleteMeasurementReportRequest>,
) -> Result<Response<pb::DeleteMeasurementReportResponse>, Status> {
    report::handle_delete_measurement_report(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn promote_report(
    api: &Api,
    request: Request<pb::PromoteMeasurementReportRequest>,
) -> Result<Response<pb::PromoteMeasurementReportResponse>, Status> {
    report::handle_promote_measurement_report(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn revoke_report(
    api: &Api,
    request: Request<pb::RevokeMeasurementReportRequest>,
) -> Result<Response<pb::RevokeMeasurementReportResponse>, Status> {
    report::handle_revoke_measurement_report(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_report_for_id(
    api: &Api,
    request: Request<pb::ShowMeasurementReportForIdRequest>,
) -> Result<Response<pb::ShowMeasurementReportForIdResponse>, Status> {
    report::handle_show_measurement_report_for_id(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_reports_for_machine(
    api: &Api,
    request: Request<pb::ShowMeasurementReportsForMachineRequest>,
) -> Result<Response<pb::ShowMeasurementReportsForMachineResponse>, Status> {
    report::handle_show_measurement_reports_for_machine(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_reports(
    api: &Api,
    request: Request<pb::ShowMeasurementReportsRequest>,
) -> Result<Response<pb::ShowMeasurementReportsResponse>, Status> {
    report::handle_show_measurement_reports(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_report(
    api: &Api,
    request: Request<pb::ListMeasurementReportRequest>,
) -> Result<Response<pb::ListMeasurementReportResponse>, Status> {
    report::handle_list_measurement_report(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn match_report(
    api: &Api,
    request: Request<pb::MatchMeasurementReportRequest>,
) -> Result<Response<pb::MatchMeasurementReportResponse>, Status> {
    report::handle_match_measurement_report(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn create_bundle(
    api: &Api,
    request: Request<pb::CreateMeasurementBundleRequest>,
) -> Result<Response<pb::CreateMeasurementBundleResponse>, Status> {
    bundle::handle_create_measurement_bundle(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn delete_bundle(
    api: &Api,
    request: Request<pb::DeleteMeasurementBundleRequest>,
) -> Result<Response<pb::DeleteMeasurementBundleResponse>, Status> {
    bundle::handle_delete_measurement_bundle(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn rename_bundle(
    api: &Api,
    request: Request<pb::RenameMeasurementBundleRequest>,
) -> Result<Response<pb::RenameMeasurementBundleResponse>, Status> {
    bundle::handle_rename_measurement_bundle(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn update_bundle(
    api: &Api,
    request: Request<pb::UpdateMeasurementBundleRequest>,
) -> Result<Response<pb::UpdateMeasurementBundleResponse>, Status> {
    bundle::handle_update_measurement_bundle(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_bundle(
    api: &Api,
    request: Request<pb::ShowMeasurementBundleRequest>,
) -> Result<Response<pb::ShowMeasurementBundleResponse>, Status> {
    bundle::handle_show_measurement_bundle(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_bundles(
    api: &Api,
    request: Request<pb::ShowMeasurementBundlesRequest>,
) -> Result<Response<pb::ShowMeasurementBundlesResponse>, Status> {
    bundle::handle_show_measurement_bundles(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_bundles(
    api: &Api,
    request: Request<pb::ListMeasurementBundlesRequest>,
) -> Result<Response<pb::ListMeasurementBundlesResponse>, Status> {
    bundle::handle_list_measurement_bundles(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_bundle_machines(
    api: &Api,
    request: Request<pb::ListMeasurementBundleMachinesRequest>,
) -> Result<Response<pb::ListMeasurementBundleMachinesResponse>, Status> {
    bundle::handle_list_measurement_bundle_machines(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn find_closest_bundle_match(
    api: &Api,
    request: Request<pb::FindClosestBundleMatchRequest>,
) -> Result<Response<pb::ShowMeasurementBundleResponse>, Status> {
    bundle::handle_find_closest_match(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn delete_journal(
    api: &Api,
    request: Request<pb::DeleteMeasurementJournalRequest>,
) -> Result<Response<pb::DeleteMeasurementJournalResponse>, Status> {
    journal::handle_delete_measurement_journal(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_journal(
    api: &Api,
    request: Request<pb::ShowMeasurementJournalRequest>,
) -> Result<Response<pb::ShowMeasurementJournalResponse>, Status> {
    journal::handle_show_measurement_journal(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_journals(
    api: &Api,
    request: Request<pb::ShowMeasurementJournalsRequest>,
) -> Result<Response<pb::ShowMeasurementJournalsResponse>, Status> {
    journal::handle_show_measurement_journals(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_journal(
    api: &Api,
    request: Request<pb::ListMeasurementJournalRequest>,
) -> Result<Response<pb::ListMeasurementJournalResponse>, Status> {
    journal::handle_list_measurement_journal(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn attest_candidate_machine(
    api: &Api,
    request: Request<pb::AttestCandidateMachineRequest>,
) -> Result<Response<pb::AttestCandidateMachineResponse>, Status> {
    machine::handle_attest_candidate_machine(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_candidate_machine(
    api: &Api,
    request: Request<pb::ShowCandidateMachineRequest>,
) -> Result<Response<pb::ShowCandidateMachineResponse>, Status> {
    machine::handle_show_candidate_machine(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn show_candidate_machines(
    api: &Api,
    request: Request<pb::ShowCandidateMachinesRequest>,
) -> Result<Response<pb::ShowCandidateMachinesResponse>, Status> {
    machine::handle_show_candidate_machines(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_candidate_machines(
    api: &Api,
    request: Request<pb::ListCandidateMachinesRequest>,
) -> Result<Response<pb::ListCandidateMachinesResponse>, Status> {
    machine::handle_list_candidate_machines(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn import_site_measurements(
    api: &Api,
    request: Request<pb::ImportSiteMeasurementsRequest>,
) -> Result<Response<pb::ImportSiteMeasurementsResponse>, Status> {
    site::handle_import_site_measurements(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn export_site_measurements(
    api: &Api,
    request: Request<pb::ExportSiteMeasurementsRequest>,
) -> Result<Response<pb::ExportSiteMeasurementsResponse>, Status> {
    site::handle_export_site_measurements(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn add_trusted_machine(
    api: &Api,
    request: Request<pb::AddMeasurementTrustedMachineRequest>,
) -> Result<Response<pb::AddMeasurementTrustedMachineResponse>, Status> {
    site::handle_add_measurement_trusted_machine(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn remove_trusted_machine(
    api: &Api,
    request: Request<pb::RemoveMeasurementTrustedMachineRequest>,
) -> Result<Response<pb::RemoveMeasurementTrustedMachineResponse>, Status> {
    site::handle_remove_measurement_trusted_machine(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_trusted_machines(
    api: &Api,
    request: Request<pb::ListMeasurementTrustedMachinesRequest>,
) -> Result<Response<pb::ListMeasurementTrustedMachinesResponse>, Status> {
    site::handle_list_measurement_trusted_machines(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn add_trusted_profile(
    api: &Api,
    request: Request<pb::AddMeasurementTrustedProfileRequest>,
) -> Result<Response<pb::AddMeasurementTrustedProfileResponse>, Status> {
    site::handle_add_measurement_trusted_profile(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn remove_trusted_profile(
    api: &Api,
    request: Request<pb::RemoveMeasurementTrustedProfileRequest>,
) -> Result<Response<pb::RemoveMeasurementTrustedProfileResponse>, Status> {
    site::handle_remove_measurement_trusted_profile(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_trusted_profiles(
    api: &Api,
    request: Request<pb::ListMeasurementTrustedProfilesRequest>,
) -> Result<Response<pb::ListMeasurementTrustedProfilesResponse>, Status> {
    site::handle_list_measurement_trusted_profiles(api, request.into_inner())
        .await
        .map(Response::new)
}

pub(crate) async fn list_attestation_summary(
    api: &Api,
    request: Request<pb::ListAttestationSummaryRequest>,
) -> Result<Response<pb::ListAttestationSummaryResponse>, Status> {
    site::handle_list_attestation_summary(api, request.into_inner())
        .await
        .map(Response::new)
}
