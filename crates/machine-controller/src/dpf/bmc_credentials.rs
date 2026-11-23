/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_dpf::repository::BmcCredentialRepository;
use carbide_dpf::{DpfError, DpfSdk};
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use mac_address::MacAddress;

/// Publish the stored DPU BMC password, independent of site rotation bookkeeping.
pub(super) async fn ensure_device_bmc_credential<R: BmcCredentialRepository, L>(
    sdk: &DpfSdk<R, L>,
    credential_reader: &dyn CredentialReader,
    device_id: &str,
) -> Result<String, DpfError> {
    let device_mac: MacAddress = device_id.parse().map_err(|_| {
        DpfError::InvalidState("DPU device ID is not a BMC MAC address".to_string())
    })?;
    let key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: device_mac,
        },
    };
    let Some(Credentials::UsernamePassword { password, .. }) =
        credential_reader.get_credentials(&key).await.map_err(|_| {
            DpfError::BmcPasswordSourceUnavailable(
                "cannot read the stored DPU BMC credential".to_string(),
            )
        })?
    else {
        return Err(DpfError::BmcPasswordSourceUnavailable(
            "stored DPU BMC credential is unavailable".to_string(),
        ));
    };
    if password.is_empty() {
        return Err(DpfError::BmcPasswordSourceUnavailable(
            "stored DPU BMC password is empty".to_string(),
        ));
    }

    // Retained credentials and a hardware update awaiting DB promotion need not
    // match a recorded site version. The Secret's revision counts publications.
    sdk.ensure_current_bmc_credential_secret(device_id, &password)
        .await
        .map_err(|error| {
            if error.is_already_exists_or_conflict()
                || matches!(&error, DpfError::NotFound { kind: "Secret", .. })
            {
                DpfError::BmcCredentialPublicationRetry(Box::new(error))
            } else {
                error
            }
        })
}

#[cfg(test)]
mod publication_tests;
