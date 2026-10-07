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

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use db::attestation as db_attest;
use db::attestation::ek_cert_verification_status;
use model::hardware_info::TpmEkCertificate;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::ParsedExtension;
use x509_parser::oid_registry;
use x509_parser::prelude::{FromDer, GeneralName};

use crate::attestation::get_ek_cert_by_machine_id;
use crate::{CarbideError, CarbideResult};

pub(crate) fn extract_ca_fields(
    ca_cert_bytes: &[u8],
) -> CarbideResult<(DateTime<Utc>, DateTime<Utc>, Vec<u8>)> {
    let ca_cert = X509Certificate::from_der(ca_cert_bytes)
        .map_err(|e| CarbideError::InvalidArgument(format!("could not parse CA cert: {e}")))?
        .1;

    Ok((
        DateTime::<Utc>::from_timestamp(ca_cert.validity.not_before.timestamp(), 0).ok_or(
            CarbideError::internal("could not parse CA's NOT BEFORE field".to_string()),
        )?,
        DateTime::<Utc>::from_timestamp(ca_cert.validity.not_after.timestamp(), 0).ok_or(
            CarbideError::internal("could not parse CA's NOT AFTER field".to_string()),
        )?,
        (*(ca_cert.subject.as_raw())).to_vec(),
    ))
}

pub(crate) async fn match_insert_new_ek_cert_status_against_ca(
    txn: &mut PgConnection,
    tpm_ek_cert: &TpmEkCertificate,
    machine_id: &MachineId,
) -> CarbideResult<()> {
    let ek_cert = X509Certificate::from_der(tpm_ek_cert.as_bytes())
        .map_err(|e| CarbideError::InvalidArgument(format!("could not parse EK cert: {e}")))?
        .1;

    // get the issuer
    let ek_issuer_bytes = (*(ek_cert.issuer.as_raw())).to_vec();

    // try obtaining the relevant CA cert from the DB and check the signature
    let mut found_signing_ca = false;
    let mut ca_id: Option<i32> = None;
    match db_attest::tpm_ca_certs::get_by_subject(txn, ek_issuer_bytes.as_slice()).await? {
        Some(ca_cert_db_entry) => {
            let ca_cert = X509Certificate::from_der(ca_cert_db_entry.ca_cert_der.as_slice())
                .map_err(|e| {
                    CarbideError::InvalidArgument(format!("could not parse CA cert: {e}"))
                })?
                .1;

            match ek_cert.verify_signature(Some(ca_cert.public_key())) {
                Ok(()) => {
                    found_signing_ca = true;
                    ca_id = Some(ca_cert_db_entry.id);
                }
                Err(e) => tracing::error!(
                    ek_certificate_serial = %ek_cert.raw_serial_as_string(),
                    issuer = %ek_cert.issuer.to_string(),
                    subject = %ca_cert.subject.to_string(),
                    error = %e,
                    "Could not verify signature for EK certificate",
                ),
            }
        }
        None => tracing::info!(
            ek_certificate_serial = %ek_cert.raw_serial_as_string(),
            issuer = %ek_cert.issuer.to_string(),
            "No CA certificate found for EK certificate",
        ),
    }

    // try and find the ek cert by its hash value
    let mut hasher = Sha256::new();
    hasher.update(tpm_ek_cert.as_bytes());
    let tpm_ek_cert_sha256 = hasher.finalize();

    if ek_cert_verification_status::get_by_ek_sha256(txn, &tpm_ek_cert_sha256)
        .await?
        .is_some()
    {
        // the entry exists, we just need to update if it was CA verified or not
        ek_cert_verification_status::update_ca_verification_status(
            txn,
            &tpm_ek_cert_sha256,
            found_signing_ca,
            ca_id,
        )
        .await?;

        tracing::info!(
            found_signing_ca,
            ek_certificate_serial = %ek_cert.raw_serial_as_string(),
            issuer = %ek_cert.issuer.to_string(),
            "Set CA verification status for EK certificate",
        );
    } else {
        // we must insert the new entry entirely

        // try to extract the URL of the CA, if present
        let mut auth_info_access_str: &str = "Authority Information Access X.509 Extension (1.3.6.1.5.5.7.1.1) URI is not present in the EK certificate";
        if let Some(auth_info_access_ext) = ek_cert
            .get_extension_unique(&oid_registry::OID_PKIX_AUTHORITY_INFO_ACCESS)
            .ok()
            .flatten()
            && let ParsedExtension::AuthorityInfoAccess(auth_info_access) =
                auth_info_access_ext.parsed_extension()
        {
            //access_methods.contains_key(oid_registry::OID_PKIX_ACCESS_DESCRIPTOR_CA_ISSUERS)
            if let Some(access_values) = auth_info_access
                .as_hashmap()
                .get(&oid_registry::OID_PKIX_ACCESS_DESCRIPTOR_CA_ISSUERS)
            {
                for access_value in access_values {
                    if let GeneralName::URI(access_uri) = access_value {
                        auth_info_access_str = access_uri;
                    }
                }
            }
        }

        let _inserted = ek_cert_verification_status::insert(
            txn,
            &tpm_ek_cert_sha256,
            &ek_cert.raw_serial_as_string(),
            found_signing_ca,
            ca_id,
            ek_cert.issuer.as_raw(),
            auth_info_access_str,
            *machine_id,
        )
        .await?;

        tracing::info!(
            ek_certificate_serial = %ek_cert.raw_serial_as_string(),
            issuer = %ek_cert.issuer.to_string(),
            found_signing_ca,
            "Added new CA verification status for EK certificate",
        );
    }

    Ok(())
}

// returns true if ek cert has been matched and status was updated, false otherwise
pub(crate) async fn match_update_existing_ek_cert_status_against_ca(
    txn: &mut PgConnection,
    ca_id: i32,
    ca_cert_bytes: &[u8],
    machine_id: &MachineId,
    ek_cert_sha256: &[u8],
) -> CarbideResult<bool> {
    // get EK cert from machine table
    let tpm_ek_cert = get_ek_cert_by_machine_id(txn, machine_id).await?;

    // create X509 EK cert
    let ek_cert = X509Certificate::from_der(tpm_ek_cert.as_bytes())
        .map_err(|e| CarbideError::internal(format!("could not parse EK cert: {e}")))?
        .1;

    // create X509 CA cert
    let ca_cert = X509Certificate::from_der(ca_cert_bytes)
        .map_err(|e| CarbideError::internal(format!("could not parse CA cert: {e}")))?
        .1;

    // verify signature
    if let Err(e) = ek_cert.verify_signature(Some(ca_cert.public_key())) {
        tracing::error!(
            ek_certificate_serial = %ek_cert.raw_serial_as_string(),
            issuer = %ek_cert.issuer.to_string(),
            subject = %ca_cert.subject.to_string(),
            error = %e,
            "Could not verify signature for EK certificate",
        );
        return Ok(false); // nothing more to do here
    }

    // update the DB
    ek_cert_verification_status::update_ca_verification_status(
        txn,
        ek_cert_sha256,
        true,
        Some(ca_id),
    )
    .await
    .map_err(|e| {
        CarbideError::internal(format!(
            "could not update CA verification status for EK serial - {}, issuer - {}, error: {}",
            ek_cert.raw_serial_as_string(),
            ek_cert.issuer,
            e
        ))
    })?;

    tracing::info!(
        ek_certificate_serial = %ek_cert.raw_serial_as_string(),
        issuer = %ek_cert.issuer.to_string(),
        "Set CA verification status for EK certificate",
    );

    Ok(true)
}
