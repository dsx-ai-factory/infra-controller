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

use std::fs;
use std::fs::File;
use std::io::Write;
use std::process::Command;

use byteorder::{BigEndian, ByteOrder};
use carbide_machine_controller::{MeasuringOutcome, handle_measuring_state};
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementReportId;
use chrono::Utc;
use db::db_read::DbReader;
use model::machine::MeasuringState;
use pkcs1::LineEnding;
use rsa::pkcs1::EncodeRsaPublicKey;
use sqlx::PgConnection;
use temp_dir::TempDir;

use crate::attestation::get_ek_cert_by_machine_id;
use crate::{CarbideError, CarbideResult};

/// VerifyQuoteState is a simple enum used to track
/// the state of a verify_quote call, specifically as
/// it relates to verifying the signature and PCR hash.
/// It is used for appropriate logging and error handling.
enum VerifyQuoteState {
    Success,
    SignatureInvalid,
    VerifyHashNoMatch,
    CompleteFailure,
}

/// Which check (or both) of a measured-boot quote failed verification.
/// `VerificationError` covers a quote whose checks could not even run --
/// malformed AK, signature, or attestation bytes, or an unsupported
/// signature type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub(crate) enum MeasuredBootVerificationFailureCause {
    SignatureInvalid,
    HashMismatch,
    SignatureAndHashMismatch,
    VerificationError,
}

/// A measured-boot quote failed verification: the PCR signature, the PCR
/// hash, or both did not check out -- or the checks themselves errored --
/// and the attestation was rejected. The full TPM event log rides the log
/// line as context, as these warn lines always included it; `error` holds
/// the underlying reason when the checks errored.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "measured_boot_verification_failed",
    metric_name = "carbide_measured_boot_verification_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "measured boot quote verification failed",
    describe = "Number of measured boot verification failures, across quote verification and attestation handling, by cause"
)]
pub(crate) struct MeasuredBootVerificationFailed {
    #[label]
    pub(crate) cause: MeasuredBootVerificationFailureCause,
    #[context]
    pub(crate) event_log: String,
    #[context]
    pub(crate) error: String,
}

impl VerifyQuoteState {
    fn from_results(signature_valid: bool, pcr_hash_matches: bool) -> Self {
        match (signature_valid, pcr_hash_matches) {
            (true, true) => Self::Success,
            (false, true) => Self::SignatureInvalid,
            (true, false) => Self::VerifyHashNoMatch,
            (false, false) => Self::CompleteFailure,
        }
    }
}

/// verify_quote_state takes the input signature validity,
/// PCR hash matching result, and a reference to the event
/// log, and will check to see if things are good (or if an
/// error needs to be returned + the event log dumped to log).
pub(crate) fn verify_quote_state(
    signature_valid: bool,
    pcr_hash_matches: bool,
    event_log: &Option<Vec<u8>>,
) -> Result<(), CarbideError> {
    let quote_state = VerifyQuoteState::from_results(signature_valid, pcr_hash_matches);
    match quote_state {
        VerifyQuoteState::Success => Ok(()),
        VerifyQuoteState::SignatureInvalid => {
            carbide_instrument::emit(MeasuredBootVerificationFailed {
                cause: MeasuredBootVerificationFailureCause::SignatureInvalid,
                event_log: event_log_to_string(event_log),
                error: String::new(),
            });
            Err(CarbideError::AttestQuoteError(
                "PCR signature invalid (see logs for full event log)".to_string(),
            ))
        }
        VerifyQuoteState::VerifyHashNoMatch => {
            carbide_instrument::emit(MeasuredBootVerificationFailed {
                cause: MeasuredBootVerificationFailureCause::HashMismatch,
                event_log: event_log_to_string(event_log),
                error: String::new(),
            });
            Err(CarbideError::AttestQuoteError(
                "PCR hash does not match (see logs for full event log)".to_string(),
            ))
        }
        VerifyQuoteState::CompleteFailure => {
            carbide_instrument::emit(MeasuredBootVerificationFailed {
                cause: MeasuredBootVerificationFailureCause::SignatureAndHashMismatch,
                event_log: event_log_to_string(event_log),
                error: String::new(),
            });
            Err(CarbideError::AttestQuoteError(
                "PCR signature invalid and PCR hash mismatch (see logs for full event log)"
                    .to_string(),
            ))
        }
    }
}

pub(crate) fn cli_make_cred(
    pub_key: rsa::RsaPublicKey,
    ak_name_serialized: &Vec<u8>,
    session_key: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CarbideError> {
    // now construct the temp directory
    let tmp_dir = TempDir::with_prefix("make_cred")
        .map_err(|e| CarbideError::AttestBindKeyError(format!("could not create TempDir: {e}")))?;
    let tmp_dir_path = tmp_dir.path();

    // create a file to write the EK key to
    let ek_file_path = tmp_dir_path.join("ek.dat");
    let mut ek_file = File::create(ek_file_path.clone())
        .map_err(|e| CarbideError::AttestBindKeyError(format!("could not create EK file: {e}")))?;

    // serialize the public key to a PEM format and write it to the file
    let pem_pub_key = pub_key.to_pkcs1_pem(LineEnding::default()).map_err(|e| {
        CarbideError::AttestBindKeyError(format!(
            "could not convert EK RsaPublicKey to PEM format: {e}"
        ))
    })?;

    ek_file.write_all(pem_pub_key.as_bytes()).map_err(|e| {
        CarbideError::AttestBindKeyError(format!("could not write EK pub to PEM file: {e}"))
    })?;

    // now write AK name to the file in hexadecimal format
    let ak_name_hex = hex::encode(ak_name_serialized);

    let session_key_path = tmp_dir_path.join("session_key.dat");
    let session_key_path_str =
        session_key_path
            .to_str()
            .ok_or(CarbideError::AttestBindKeyError(
                "could not join seession_key_path".to_string(),
            ))?;

    let mut session_key_file = File::create(session_key_path.clone()).map_err(|e| {
        CarbideError::AttestBindKeyError(format!("could not create file for session key: {e}"))
    })?;
    session_key_file.write_all(session_key).map_err(|e| {
        CarbideError::AttestBindKeyError(format!("could not write session key to file: {e}"))
    })?;

    // construct the command to execute make_credential
    let ek_file_path_str = ek_file_path
        .to_str()
        .ok_or(CarbideError::AttestBindKeyError(
            "could not convert ek_file_path to str".to_string(),
        ))?;

    let cred_out_path = tmp_dir_path.join("mkcred.out");
    let cred_out_path_str = cred_out_path
        .to_str()
        .ok_or(CarbideError::AttestBindKeyError(
            "could not join cred_out_path".to_string(),
        ))?;

    let cmd_str = format!(
        "tpm2 makecredential -u {ek_file_path_str} -s {session_key_path_str} -n {ak_name_hex} -o {cred_out_path_str} -G rsa -V --tcti=none"
    );

    tracing::debug!(
        command = %cmd_str,
        "make credential command",
    );
    // execute the makecredential command
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .output()
        .map_err(|e| {
            CarbideError::AttestBindKeyError(format!(
                "could not execute makecredential command: {e}"
            ))
        })?;

    if !output.stderr.is_empty() {
        tracing::error!(
            error = %String::from_utf8_lossy(output.stderr.as_slice()),
            "tpm2 makecredential returned error",
        );
    }

    let creds = fs::read(cred_out_path).map_err(|e| {
        CarbideError::AttestBindKeyError(format!("could not create creds file: {e}"))
    })?;

    let (cred_blob, encr_secret) = extract_cred_secret(&creds)?;

    Ok((cred_blob, encr_secret))
}

#[cfg_attr(not(feature = "linux-build"), allow(unused_variables))]
pub(crate) fn verify_signature(
    ak_pub: &[u8],
    attest_vec: &[u8],
    rsa_signature: &[u8],
) -> CarbideResult<bool> {
    #[cfg(feature = "linux-build")]
    {
        use tss_esapi::structures::{Public, Signature};
        use tss_esapi::traits::UnMarshall;

        let ak_pub = Public::unmarshall(ak_pub).map_err(|e| {
            CarbideError::AttestQuoteError(format!("could not unmarshal AK pub: {e}"))
        })?;

        let signature = Signature::unmarshall(rsa_signature).map_err(|e| {
            CarbideError::AttestQuoteError(format!("could not unmarshall signature struct: {e}"))
        })?;

        linux_build::verify_signature(&ak_pub, attest_vec, &signature)
    }
    #[cfg(not(feature = "linux-build"))]
    {
        Err(attestation_unsupported_error())
    }
}

#[cfg_attr(not(feature = "linux-build"), allow(unused_variables))]
pub(crate) fn verify_pcr_hash(attestation: &[u8], pcr_values: &[Vec<u8>]) -> CarbideResult<bool> {
    #[cfg(feature = "linux-build")]
    {
        use tss_esapi::structures::Attest;
        use tss_esapi::traits::UnMarshall;
        let attest = Attest::unmarshall(attestation).map_err(|e| {
            CarbideError::AttestQuoteError(format!("could not unmarshall attest struct: {e}"))
        })?;
        linux_build::verify_pcr_hash(&attest, pcr_values)
    }
    #[cfg(not(feature = "linux-build"))]
    {
        Err(attestation_unsupported_error())
    }
}

fn extract_cred_secret(creds: &[u8]) -> CarbideResult<(Vec<u8>, Vec<u8>)> {
    let magic_header_offset: usize = 8; // 4 bytes for magic number and 4 bytes for version

    // get length for cred blob
    // read cred blob
    let cred_blob_offset: usize = 2;
    let secret_offset: usize = 2;

    if creds.len() < magic_header_offset + cred_blob_offset {
        return Err(CarbideError::AttestBindKeyError(format!(
            "creds file is too short: {0} bytes",
            creds.len()
        )));
    }

    let cred_blob_size_bytes =
        &creds[magic_header_offset..(magic_header_offset + cred_blob_offset)];
    let cred_blob_size = BigEndian::read_u16(cred_blob_size_bytes);

    let cred_blob_end_idx: usize =
        magic_header_offset + cred_blob_offset + usize::from(cred_blob_size);

    if creds.len() < cred_blob_end_idx + secret_offset - 1 {
        return Err(CarbideError::AttestBindKeyError(format!(
            "creds file is too short: {0} bytes",
            creds.len()
        )));
    }
    let cred_blob = Vec::from(&creds[magic_header_offset + cred_blob_offset..cred_blob_end_idx]);

    // read secret
    let secret = Vec::from(&creds[cred_blob_end_idx + secret_offset..]);

    Ok((cred_blob, secret))
}

/// event_log_to_string converts the input event log (which
/// comes to us via the proto as an Option<Vec<u8>) into a String,
/// for passing to tracing/logging.
///
/// since the event log is currently "best effort", we'll log a
/// little "error" in <>'s if we notice there's no event log.
pub(crate) fn event_log_to_string(event_log: &Option<Vec<u8>>) -> String {
    event_log
        .as_ref()
        .map(|log_utf8| {
            String::from_utf8(log_utf8.to_vec())
                .unwrap_or(String::from("<event log failed utf8 conversion>"))
        })
        .unwrap_or(String::from("<event log empty>"))
}

#[cfg_attr(not(feature = "linux-build"), allow(unused_variables))]
pub(crate) async fn compare_pub_key_against_cert(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    ek_pub: &[u8],
) -> CarbideResult<(bool, rsa::RsaPublicKey)> {
    let tpm_ek_cert = get_ek_cert_by_machine_id(txn, machine_id).await?;
    #[cfg(feature = "linux-build")]
    {
        linux_build::do_compare_pub_key_against_cert(&tpm_ek_cert, ek_pub)
    }
    #[cfg(not(feature = "linux-build"))]
    {
        Err(attestation_unsupported_error())
    }
}

pub(crate) async fn has_passed_attestation<DB>(
    db: &mut DB,
    machine_id: &MachineId,
    _report_id: &MeasurementReportId,
) -> CarbideResult<bool>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let measuring_outcome = handle_measuring_state(
        &MeasuringState::WaitingForMeasurements,
        machine_id,
        db,
        true,
        Utc::now(),
        chrono::Duration::MAX,
    )
    .await
    .map_err(|e| CarbideError::AttestQuoteError(e.to_string()))?;

    Ok(measuring_outcome == MeasuringOutcome::PassedOk)
}

#[cfg(not(feature = "linux-build"))]
fn attestation_unsupported_error() -> CarbideError {
    CarbideError::AttestQuoteError("this server does not support attestation".to_string())
}

#[cfg(feature = "linux-build")]
pub(crate) mod linux_build {
    use asn1_rs::FromDer;
    use model::hardware_info::TpmEkCertificate;
    use rsa::{BigUint, RsaPublicKey};
    use sha2::Digest;
    use tss_esapi::structures::Signature::RsaPss;
    use tss_esapi::structures::{Attest, AttestInfo, Public, Signature};
    use tss_esapi::traits::UnMarshall;
    use x509_parser::certificate::X509Certificate;
    use x509_parser::public_key::PublicKey as x509_parser_pub_key;

    use crate::attestation::digest_crate_shim::Sha256LegacyDigestShim;
    use crate::errors::{CarbideError, CarbideResult};

    const RSA_PUBKEY_EXPONENT: u32 = 65537u32;

    pub(super) fn verify_pcr_hash(attest: &Attest, pcr_values: &[Vec<u8>]) -> CarbideResult<bool> {
        let attest_digest = match attest.attested() {
            AttestInfo::Quote { info } => info.pcr_digest(),
            _other => {
                return Err(CarbideError::AttestQuoteError(
                    "incorrect attestation type".into(),
                ));
            }
        };

        let mut hasher = sha2::Sha256::new();

        pcr_values.iter().for_each(|buf| {
            hasher.update(buf);
        });

        let computed_pcr_hash = hasher.finalize();

        if attest_digest.value() == computed_pcr_hash.as_slice() {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn do_compare_pub_key_against_cert(
        tpm_ek_cert: &TpmEkCertificate,
        ek_pub: &[u8],
    ) -> CarbideResult<(bool, rsa::RsaPublicKey)> {
        // compare the pub key and the cert

        let cert = X509Certificate::from_der(tpm_ek_cert.as_bytes())
            .map_err(|e| {
                CarbideError::AttestBindKeyError(format!("could not unmarshall EK cert: {e}"))
            })?
            .1;

        let pub_key_cert_data = cert.public_key().parsed().map_err(|e| {
            CarbideError::AttestBindKeyError(format!("could not get EK cert data: {e}"))
        })?;

        let ek_cert_modulus = match pub_key_cert_data {
            x509_parser_pub_key::RSA(rsa_pub_key) => rsa_pub_key.modulus,
            _rest => {
                return Err(CarbideError::AttestBindKeyError(
                    "TPM EK is not in RSA format".to_string(),
                ));
            }
        };

        // now, we construct the actual public key from the modulus and exponent
        let modulus = BigUint::from_bytes_be(ek_cert_modulus);
        let exponent: BigUint = BigUint::from(RSA_PUBKEY_EXPONENT);

        // pub_key_cert has a different type from pub_key_cert_data, even though their type names
        // actually do coincide!
        let pub_key_cert = RsaPublicKey::new(modulus, exponent).map_err(|e| {
            CarbideError::AttestBindKeyError(format!(
                "could not create RsaPublicKey from EK cert: {e}"
            ))
        })?;
        // construct the Public structure and extract the PublicKeyRsa from it, which is really just the modulus
        let ek_pub = Public::unmarshall(ek_pub).map_err(|e| {
            CarbideError::AttestBindKeyError(format!("could not unmarshall EK: {e}"))
        })?;

        let unique = match ek_pub {
            Public::Rsa { unique, .. } => unique,
            _ => {
                return Err(CarbideError::AttestBindKeyError(
                    "EK pub is not in RSA format".to_string(),
                ));
            }
        };

        // now, we construct the actual public key from the modulus and exponent
        let modulus = BigUint::from_bytes_be(unique.value());
        let exponent: BigUint = BigUint::from(RSA_PUBKEY_EXPONENT);

        let pub_key_ek = RsaPublicKey::new(modulus, exponent).map_err(|e| {
            CarbideError::AttestBindKeyError(format!(
                "could not create RsaPublicKey from TPM's EK pub: {e}"
            ))
        })?;

        Ok((pub_key_ek == pub_key_cert, pub_key_ek))
    }

    pub(super) fn verify_signature(
        ak_pub: &Public,
        attest_vec: &[u8],
        signature: &Signature,
    ) -> CarbideResult<bool> {
        // let's take hash of the original attestation
        let mut hasher = sha2::Sha256::new();
        hasher.update(attest_vec);
        let attest_hash = hasher.finalize();

        let unique = match ak_pub {
            tss_esapi::structures::Public::Rsa { unique, .. } => unique,
            _ => {
                return Err(CarbideError::AttestQuoteError(
                    "AK pub is not an RSA key".to_string(),
                ));
            }
        };

        // now, we construct the actual public key from the modulus and exponent
        let modulus = BigUint::from_bytes_be(unique.value());
        let exponent: BigUint = BigUint::from(RSA_PUBKEY_EXPONENT);

        let pub_key = RsaPublicKey::new(modulus, exponent).map_err(|e| {
            CarbideError::AttestQuoteError(format!("could not create RsaPublicKey: {e}"))
        })?;

        let rsa_signature = match signature {
            RsaPss(rsa_signature) => rsa_signature,
            _ => {
                return Err(CarbideError::AttestQuoteError(
                    "unknown signature type".to_string(),
                ));
            }
        };

        match pub_key.verify(
            rsa::Pss::new::<Sha256LegacyDigestShim>(),
            &attest_hash,
            rsa_signature.signature().value(),
        ) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}
#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};

    use super::*;

    #[test]
    fn test_extract_cred_secret_buffer_too_short_panics() {
        let creds = [12, 13, 15];
        let res = extract_cred_secret(&creds);

        match res {
            Ok(..) => panic!("Failed: Should have received an error"),
            Err(e) => assert_eq!(
                e.to_string(),
                "attest bind key error: creds file is too short: 3 bytes"
            ),
        }
    }

    /// Every failing verification writes one WARN line (the event log as a
    /// field) AND moves the failure counter under its cause label; a passing
    /// verification emits nothing. The `verification_error` cause -- the
    /// checks themselves erroring in `attest_quote` -- rides the same event
    /// with the underlying reason as the `error` field.
    #[test]
    fn verification_failure_logs_and_counts_by_cause() {
        let event_log = Some(b"tpm event log".to_vec());

        // The quote-failure DB tests each drive one cause once, from
        // seconds-long flows that do not hold the capture lock -- the odds of
        // one landing inside this capture window are negligible.
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            assert!(verify_quote_state(true, true, &event_log).is_ok());
            assert!(verify_quote_state(false, true, &event_log).is_err());
            assert!(verify_quote_state(true, false, &event_log).is_err());
            assert!(verify_quote_state(false, false, &event_log).is_err());
            carbide_instrument::emit(MeasuredBootVerificationFailed {
                cause: MeasuredBootVerificationFailureCause::VerificationError,
                event_log: event_log_to_string(&event_log),
                error: "PCR signature verification failed: bad AK".to_string(),
            });
        });

        assert_eq!(logs.len(), 4, "the success case must not log");
        let field = |log: &carbide_instrument::testing::CapturedLog, name: &str| {
            log.fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        for (log, cause_label, error) in [
            (&logs[0], "signature_invalid", ""),
            (&logs[1], "hash_mismatch", ""),
            (&logs[2], "signature_and_hash_mismatch", ""),
            (
                &logs[3],
                "verification_error",
                "PCR signature verification failed: bad AK",
            ),
        ] {
            assert_eq!(log.level, tracing::Level::WARN, "cause {cause_label}");
            assert_eq!(log.message, "measured boot quote verification failed");
            assert_eq!(field(log, "cause"), Some(cause_label.to_string()));
            assert_eq!(field(log, "event_log"), Some("tpm event log".to_string()));
            assert_eq!(field(log, "error"), Some(error.to_string()));

            assert_eq!(
                metrics.counter_delta(
                    "carbide_measured_boot_verification_failures_total",
                    &[("cause", cause_label)],
                ),
                1.0,
                "counter for cause={cause_label}"
            );
        }
    }
}
