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

use std::ffi::CString;
use std::process::Command;
use std::str::FromStr;
use std::vec::Vec;

use ::rpc::machine_discovery::TpmDescription;
use ::rpc::{forge as rpc, machine_discovery as rpc_md};
use carbide_uuid::machine::MachineId;
use tss_esapi::abstraction::{ak, ek};
use tss_esapi::attributes::session::SessionAttributesBuilder;
use tss_esapi::constants::{CapabilityType, PropertyTag, SessionType};
use tss_esapi::handles::{AuthHandle, KeyHandle, SessionHandle};
use tss_esapi::interface_types::algorithm::{
    AsymmetricAlgorithm, HashingAlgorithm, SignatureSchemeAlgorithm,
};
use tss_esapi::interface_types::session_handles::PolicySession;
use tss_esapi::structures::CapabilityData::TpmProperties;
use tss_esapi::structures::{
    Attest, Data, Digest, EncryptedSecret, HashScheme, IdObject, PcrSelectionListBuilder, PcrSlot,
    Signature, SignatureScheme, SymmetricDefinition,
};
use tss_esapi::traits::Marshall;
use tss_esapi::{Context, TctiNameConf};

use crate::CarbideClientError;

pub(super) fn create_context_from_path(path: &str) -> Result<Context, Box<dyn std::error::Error>> {
    let tcti = TctiNameConf::from_str(path)?;
    // create context
    let ctx = Context::new(tcti)?;
    Ok(ctx)
}

pub(super) fn create_attest_key_info(
    ctx: &mut Context,
) -> Result<(rpc_md::AttestKeyInfo, KeyHandle, KeyHandle), Box<dyn std::error::Error>> {
    // obtain EK
    let ek_handle = ek::create_ek_object(ctx, AsymmetricAlgorithm::Rsa, None)?;
    tracing::debug!("Obtained EK handle");
    // create AK
    let ak = ak::create_ak(
        ctx,
        ek_handle,
        HashingAlgorithm::Sha256,
        SignatureSchemeAlgorithm::RsaPss,
        None,
        None,
    )?;
    // load ak - get handle, we'll need it for getting obj name and signing later
    let ak_handle = ak::load_ak(
        ctx,
        ek_handle,
        None,
        ak.out_private.clone(),
        ak.out_public.clone(),
    )?;

    tracing::debug!("Created and loaded AK");

    // read public - get ak name (cryptographic)
    let (_, ak_key_name, _) = ctx.read_public(ak_handle)?;
    let (ek_public, _, _) = ctx.read_public(ek_handle)?;

    // create rpc message now
    let attest_key_info = rpc_md::AttestKeyInfo {
        ak_pub: ak.out_public.marshall()?,
        ak_name: Vec::from(ak_key_name.value()),
        ek_pub: ek_public.marshall()?,
    };

    Ok((attest_key_info, ek_handle, ak_handle))
}

pub(super) fn activate_credential(
    cred_blob_serialized: &[u8],
    encr_secret_serialized: &[u8],
    ctx: &mut Context,
    ek_handle: &KeyHandle,
    ak_handle: &KeyHandle,
) -> Result<Digest, Box<dyn std::error::Error>> {
    // use activate credential to obtain the credential (nonce)
    let cred_blob = IdObject::try_from(cred_blob_serialized)?;
    let encr_secret = EncryptedSecret::try_from(encr_secret_serialized)?;

    // in order to call activate_credential, we need a policy auth session. this session acts as a vehicle for enforcing that
    // PolicySecret is applied, i.e. that we have access to the endorsement key
    let ek_auth_session_option = ctx.start_auth_session(
        None,
        None,
        None,
        SessionType::Policy,
        SymmetricDefinition::AES_128_CFB,
        HashingAlgorithm::Sha256,
    )?;

    let ek_auth_session = match ek_auth_session_option {
        Some(auth_session) => auth_session,
        None => {
            return Err(Box::new(CarbideClientError::TpmError(
                "Could not start auth session 1".to_string(),
            )));
        }
    };

    // hmac auth session is needed for authorising access to the ak key. please note that this is not an extra policy key, but
    // rather a separate session on specific key
    let ak_auth_session_option = ctx.start_auth_session(
        None,
        None,
        None,
        SessionType::Hmac,
        SymmetricDefinition::AES_128_CFB,
        HashingAlgorithm::Sha256,
    )?;

    let ak_auth_session = match ak_auth_session_option {
        Some(auth_session) => auth_session,
        None => {
            return Err(Box::new(CarbideClientError::TpmError(
                "Could not start auth session 2".to_string(),
            )));
        }
    };

    let (session_attributes, session_attributes_mask) = SessionAttributesBuilder::new().build();

    ctx.tr_sess_set_attributes(ek_auth_session, session_attributes, session_attributes_mask)?;
    ctx.tr_sess_set_attributes(ak_auth_session, session_attributes, session_attributes_mask)?;

    let _ = ctx.execute_with_session(ak_auth_session_option, |ctx| {
        ctx.policy_secret(
            PolicySession::try_from(ek_auth_session)?,
            AuthHandle::Endorsement,
            Default::default(),
            Default::default(),
            Default::default(),
            None,
        )
    })?;

    ctx.set_sessions((ak_auth_session_option, ek_auth_session_option, None));

    let digest = ctx.activate_credential(*ak_handle, *ek_handle, cred_blob, encr_secret)?;

    tracing::debug!("Activated credential");

    ctx.flush_context(SessionHandle::from(ak_auth_session).into())?;
    ctx.flush_context(SessionHandle::from(ek_auth_session).into())?;
    ctx.clear_sessions();

    Ok(digest)
}

fn detect_pcr_hash_algo(ctx: &mut Context) -> Result<HashingAlgorithm, Box<dyn std::error::Error>> {
    let is_sha256 = match probe_sample_pcr_value(ctx, HashingAlgorithm::Sha256) {
        Ok(val) => val,
        Err(err) => {
            tracing::error!(
                error = %err,
                "Error probing hash SHA256; setting to false",
            );
            false
        }
    };
    let is_sha384 = match probe_sample_pcr_value(ctx, HashingAlgorithm::Sha384) {
        Ok(val) => val,
        Err(err) => {
            tracing::error!(
                error = %err,
                "Error probing hash SHA384; setting to false",
            );
            false
        }
    };

    // prefer SHA256 over SHA384
    if is_sha256 {
        return Ok(HashingAlgorithm::Sha256);
    }

    if is_sha384 {
        return Ok(HashingAlgorithm::Sha384);
    }

    Err(Box::new(CarbideClientError::TpmError(
        "TPM PCR is using an unsupported hash. Only SHA256 and SHA384 are supported".to_string(),
    )))
}

fn probe_sample_pcr_value(
    ctx: &mut Context,
    probe_hash: HashingAlgorithm,
) -> Result<bool, Box<dyn std::error::Error>> {
    let pcr_selection_list = PcrSelectionListBuilder::new()
        .with_selection(probe_hash, &[PcrSlot::Slot0])
        .build()?;

    let (_, _, digest_list) = ctx.pcr_read(pcr_selection_list)?;

    Ok(!digest_list.is_empty())
}

pub(super) fn get_pcr_quote(
    ctx: &mut Context,
    ak_handle: &KeyHandle,
) -> Result<(Attest, Signature, Vec<Digest>), Box<dyn std::error::Error>> {
    // it used to be that PCR values would only be in SHA256, this can now
    // be in SHA384 also. We figure out which ones those are by probing them.
    let pcr_hash_algo = detect_pcr_hash_algo(ctx)?;
    tracing::info!(?pcr_hash_algo, "Using PCR hash");

    let ak_auth_session_option = ctx.start_auth_session(
        None,
        None,
        None,
        SessionType::Hmac,
        SymmetricDefinition::AES_128_CFB,
        HashingAlgorithm::Sha256,
    )?;

    let ak_auth_session = match ak_auth_session_option {
        Some(auth_session) => auth_session,
        None => {
            return Err(Box::new(CarbideClientError::TpmError(
                "Could not start auth session".to_string(),
            )));
        }
    };

    let (session_attributes, session_attributes_mask) = SessionAttributesBuilder::new().build();

    ctx.tr_sess_set_attributes(ak_auth_session, session_attributes, session_attributes_mask)?;

    let selection_list = PcrSelectionListBuilder::new()
        .with_selection(
            pcr_hash_algo,
            &[
                PcrSlot::Slot0,
                PcrSlot::Slot1,
                PcrSlot::Slot2,
                PcrSlot::Slot3,
                PcrSlot::Slot4,
                PcrSlot::Slot5,
                PcrSlot::Slot6,
                PcrSlot::Slot7,
                PcrSlot::Slot8,
                PcrSlot::Slot9,
                PcrSlot::Slot10,
                PcrSlot::Slot11,
            ],
        )
        .build()?;

    // this apparently means "no qualifying data" - whatever that is ...
    let qualifying_data = vec![0xff; 16];

    ctx.set_sessions((ak_auth_session_option, None, None));
    // get the quote and the signature
    let (attest, signature) = ctx.quote(
        *ak_handle,
        Data::try_from(qualifying_data)?,
        SignatureScheme::RsaPss {
            hash_scheme: HashScheme::new(HashingAlgorithm::Sha256),
        },
        selection_list.clone(),
    )?;

    tracing::debug!(?attest, "Obtained attestation");
    tracing::debug!(?signature, "Obtained signature");

    //verify_signature(ctx, &attest, &signature, &ak_handle);

    // clean up sessions as soon as we are finished with them
    ctx.clear_sessions();
    ctx.flush_context(SessionHandle::from(ak_auth_session).into())?;

    // get the actual pcr values
    let mut selection_list_mut = selection_list;
    let mut digest_vec = Vec::<Digest>::new();

    loop {
        let (_, read_list, pcr_list) = ctx.pcr_read(selection_list_mut.clone())?;
        digest_vec.extend_from_slice(pcr_list.value());

        if read_list.is_empty()
            || (read_list.len() == 1 && read_list.get_selections()[0].is_empty())
        {
            break;
        }

        selection_list_mut.subtract(&read_list)?;
    }

    if digest_vec.is_empty() {
        tracing::error!(
            "No PCR values have been read. Maybe a wrong Tpm2Algorithm had been set in BIOS?"
        );
    }

    tracing::debug!(pcr_digests = ?digest_vec, "Obtained PCR digests");

    Ok((attest, signature, digest_vec))
}

pub(super) fn create_quote_request(
    attestation: Attest,
    signature: Signature,
    pcr_values: Vec<Digest>,
    credential: &Digest,
    machine_id: &MachineId,
    tpm_eventlog: &Option<Vec<u8>>,
) -> Result<rpc::AttestQuoteRequest, Box<dyn std::error::Error>> {
    let request = rpc::AttestQuoteRequest {
        attestation: attestation.marshall()?,
        signature: signature.marshall()?,
        credential: Vec::from(credential.value()),
        pcr_values: pcr_values
            .iter()
            .map(|digest| Vec::from(digest.value()))
            .collect(),
        machine_id: Some(*machine_id),
        event_log: tpm_eventlog.clone(),
    };

    Ok(request)
}

pub(super) fn get_tpm_eventlog() -> Option<Vec<u8>> {
    let output_res = Command::new("sh")
        .arg("-c")
        .arg("tpm2_eventlog /sys/kernel/security/tpm0/binary_bios_measurements")
        .output();

    let output = match output_res {
        Ok(output) => output,
        Err(e) => {
            tracing::error!(error = %e, "Could not retrieve TPM event log");
            return None;
        }
    };

    if !output.status.success() {
        tracing::error!(
            stderr = %String::from_utf8(output.stderr)
                .unwrap_or("<could not parse stderr log>".to_string()),
            "Error retrieving TPM event log",
        );
        None
    } else {
        Some(output.stdout)
    }
}

pub(super) fn get_tpm_description(ctx: &mut Context) -> Option<TpmDescription> {
    let (capabilities, _more) = match ctx.get_capability(CapabilityType::TpmProperties, 0, 80) {
        Ok(tuple) => tuple,
        Err(e) => {
            tracing::error!(
                error = %e,
                "GetTpmDescription: Could not get TPM capability data",
            );
            return None;
        }
    };

    let tpm_properties = match capabilities {
        TpmProperties(property_list) => property_list,
        _ => {
            tracing::error!("Failed to call get TpmProperties");
            return None;
        }
    };

    let mut firmware_version_1 = 0u32;
    let mut firmware_version_2 = 0u32;
    let mut spec_version = String::default();
    let mut vendor_1 = String::default();
    let mut vendor_2 = String::default();

    for tagged_property in tpm_properties {
        match tagged_property.property() {
            // this is spec version
            PropertyTag::FamilyIndicator => {
                spec_version =
                    CString::from_vec_with_nul(tagged_property.value().to_be_bytes().to_vec())
                        .map(|s| {
                            s.into_string()
                                .unwrap_or("Could not convert spec_version".to_string())
                        })
                        .unwrap_or("Could not convert spec_version".to_string());
            }
            PropertyTag::VendorString1 => {
                vendor_1 =
                    CString::from_vec_with_nul(tagged_property.value().to_be_bytes().to_vec())
                        .map(|s| {
                            s.into_string()
                                .unwrap_or("Could not convert spec_version".to_string())
                        })
                        .unwrap_or("Could not convert spec_version".to_string());
            }
            PropertyTag::VendorString2 => {
                vendor_2 =
                    CString::from_vec_with_nul(tagged_property.value().to_be_bytes().to_vec())
                        .map(|s| {
                            s.into_string()
                                .unwrap_or("Could not convert spec_version".to_string())
                        })
                        .unwrap_or("Could not convert spec_version".to_string());
            }
            PropertyTag::FirmwareVersion1 => firmware_version_1 = tagged_property.value(),
            PropertyTag::FirmwareVersion2 => firmware_version_2 = tagged_property.value(),
            _ => (),
        }
    }

    tracing::debug!(family_indicator = %spec_version, "Read TPM family indicator");

    let vendor = vendor_1.clone() + vendor_2.as_str();
    tracing::debug!(%vendor, "Read TPM vendor");

    let firmware_version = format!("0x{firmware_version_1:x}.0x{firmware_version_2:x}");

    tracing::debug!(%firmware_version, "Read TPM firmware version");

    if firmware_version_1 == 0
        && firmware_version_2 == 0
        && vendor_1 == String::default()
        && vendor_2 == String::default()
        && spec_version == String::default()
    {
        tracing::error!("GetTpmDescription: Could not extract tpm description");
        return None;
    }

    Some(TpmDescription {
        tpm_spec: spec_version,
        vendor,
        firmware_version,
    })
}
