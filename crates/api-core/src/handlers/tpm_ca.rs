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
use db::attestation as db_attest;
use tonic::{Request, Response};
use x509_parser::prelude::FromDer;
use x509_parser::x509::X509Name;

use crate::api::{Api, log_request_data};
use crate::{CarbideError, attestation as attest};

pub(crate) async fn tpm_add_ca_cert(
    api: &Api,
    request: Request<rpc::TpmCaCert>,
) -> Result<Response<rpc::TpmCaAddedCaStatus>, tonic::Status> {
    log_request_data(&request);

    let payload = request.into_inner();
    let ca_cert_bytes = payload.ca_cert.as_slice();
    // parse ca cert, extract serial num, nvb, nva, subject (in binary)
    let (not_valid_before, not_valid_after, subject) = attest::extract_ca_fields(ca_cert_bytes)?;
    // insert cert into the DB (in binary) + all the extracted fields above
    let mut txn = api.txn_begin().await?;

    let db_ca_cert_opt = db_attest::tpm_ca_certs::insert(
        &mut txn,
        &not_valid_before,
        &not_valid_after,
        ca_cert_bytes,
        subject.as_slice(),
    )
    .await?;

    let db_ca_cert = match db_ca_cert_opt {
        Some(cert) => cert,
        None => {
            return Err(CarbideError::internal(
                "CA cert not returned on successful insertion".to_string(),
            )
            .into());
        }
    };

    // now update all the existing EK statuses
    let ek_certs =
        db_attest::ek_cert_verification_status::get_by_issuer(&mut txn, subject.as_slice()).await?;

    let mut ek_certs_updated: u32 = 0;
    if !ek_certs.is_empty() {
        for ek_cert in ek_certs {
            if attest::tpm_ca_cert::match_update_existing_ek_cert_status_against_ca(
                &mut txn,
                db_ca_cert.id,
                ca_cert_bytes,
                &ek_cert.machine_id,
                &ek_cert.ek_sha256,
            )
            .await?
            {
                ek_certs_updated += 1;
            }
        }
    }

    txn.commit().await?;

    Ok(Response::new(rpc::TpmCaAddedCaStatus {
        id: Some(rpc::TpmCaCertId {
            ca_cert_id: db_ca_cert.id,
        }),
        matched_ek_certs: ek_certs_updated as i32,
    }))
}

pub(crate) async fn tpm_show_ca_certs(
    api: &Api,
    request: &Request<()>,
) -> Result<Response<rpc::TpmCaCertDetailCollection>, tonic::Status> {
    log_request_data(request);

    let mut txn = api.txn_begin().await?;

    let ca_certs = db_attest::tpm_ca_certs::get_all(&mut txn).await?;

    txn.commit().await?;

    let ca_cert_details = ca_certs
        .iter()
        .map(|entry| rpc::TpmCaCertDetail {
            ca_cert_id: entry.id,
            not_valid_before: entry.not_valid_before.to_rfc2822(),
            not_valid_after: entry.not_valid_after.to_rfc2822(),
            ca_cert_subject: X509Name::from_der(&entry.cert_subject)
                .map(|x| x.1.to_string())
                .unwrap_or("Could not parse CA subject name".to_string()),
        })
        .collect();

    Ok(Response::new(rpc::TpmCaCertDetailCollection {
        tpm_ca_cert_details: ca_cert_details,
    }))
}

pub(crate) async fn tpm_show_unmatched_ek_certs(
    api: &Api,
    request: &Request<()>,
) -> Result<Response<rpc::TpmEkCertStatusCollection>, tonic::Status> {
    log_request_data(request);

    let mut txn = api.txn_begin().await?;

    let unmatched_ek_statuses =
        db_attest::ek_cert_verification_status::get_by_unmatched_ca(&mut txn).await?;

    txn.commit().await?;

    let unmatched_eks = unmatched_ek_statuses
        .iter()
        .map(|entry| rpc::TpmEkCertStatus {
            serial_num: entry.serial_num.clone(),
            machine_id: entry.machine_id.into(),
            issuer: X509Name::from_der(&entry.issuer)
                .map(|x| x.1.to_string())
                .unwrap_or("Could not parse issuer".to_string()),
            issuer_ca_url: entry.issuer_access_info.clone(),
        })
        .collect();

    Ok(Response::new(rpc::TpmEkCertStatusCollection {
        tpm_ek_cert_statuses: unmatched_eks,
    }))
}

pub(crate) async fn tpm_delete_ca_cert(
    api: &Api,
    request: Request<rpc::TpmCaCertId>,
) -> Result<Response<()>, tonic::Status> {
    log_request_data(&request);

    let payload = request.into_inner();
    let ca_cert_id = payload.ca_cert_id;

    let mut txn = api.txn_begin().await?;

    db_attest::ek_cert_verification_status::unmatch_ca_verification_status(&mut txn, ca_cert_id)
        .await?;

    db_attest::tpm_ca_certs::delete(&mut txn, ca_cert_id).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}
