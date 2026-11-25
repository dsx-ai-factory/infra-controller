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

//! UEFI (BIOS setup) password changes shared by ingestion, rotation, and the
//! admin RPCs.

use bmc_backend::BackendBmc;
use bmc_platform::{DriverOutcome, PlatformError};
use bmc_vendor::DpuModel;
use carbide_instrument::Instrumentation;
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, CredentialType, Credentials};

use crate::platform_config::vendor_job_id;

/// Changes the UEFI password to `new_password`, authenticating with each of
/// `candidates` in order until one is accepted.
pub async fn rotate_uefi_password_candidates(
    bmc: &BackendBmc,
    candidates: &[String],
    new_password: &str,
) -> Result<DriverOutcome, PlatformError> {
    let bios = bmc.bios()?;
    let mut last_err = None;
    for candidate in candidates {
        match bios.change_uefi_password(candidate, new_password).await {
            Ok(outcome) => return Ok(outcome),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "UEFI password change failed for a current-password candidate; trying the next"
                );
                last_err = Some(error);
            }
        }
    }
    Err(last_err.expect("rotate_uefi_password_candidates requires at least one candidate"))
}

/// Sets a host's UEFI password to `site_password`. The host is expected to have
/// no password, or `site_password` already when it was force-deleted without
/// clearing it. Returns the vendor job to wait for before restarting.
pub async fn set_host_uefi_password(
    bmc: &BackendBmc,
    site_password: &str,
) -> Result<Option<String>, PlatformError> {
    let candidates = [String::new(), site_password.to_string()];
    let outcome = rotate_uefi_password_candidates(bmc, &candidates, site_password).await?;
    Ok(vendor_job_id(&outcome).map(|job_id| job_id.to_string()))
}

/// Replaces a DPU's factory UEFI password, read through `reader`, with
/// `site_password`; the change applies on the DPU's next restart. Returns
/// `false`, having staged nothing, when the DPU exposes no UEFI password.
pub async fn set_dpu_uefi_password(
    bmc: &BackendBmc,
    reader: &dyn CredentialReader,
    site_password: &str,
    instrumentation: &Instrumentation,
) -> eyre::Result<bool> {
    let bios = bmc.bios().map_err(PlatformError::from)?;
    if !bios.dpu_exposes_uefi_password(instrumentation).await? {
        return Ok(false);
    }
    let factory_password = read_dpu_factory_default(reader)
        .await
        .map_err(|e| eyre::eyre!("read DPU UEFI factory default: {e}"))?;
    match bios
        .change_uefi_password(&factory_password, site_password)
        .await
    {
        Ok(_) => Ok(true),
        Err(PlatformError::Unsupported) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// The DPU hardware's factory UEFI password, or the well-known `bluefield`
/// default when the store has none.
pub(crate) async fn read_dpu_factory_default(
    reader: &dyn CredentialReader,
) -> Result<String, SecretsError> {
    let key = CredentialKey::DpuUefi {
        credential_type: CredentialType::DpuHardwareDefault {
            model: DpuModel::Unknown,
        },
    };
    Ok(reader
        .get_credentials(&key)
        .await?
        .map(|Credentials::UsernamePassword { password, .. }| password)
        .unwrap_or_else(|| "bluefield".to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use carbide_redfish::libredfish::test_support::{RedfishSim, RedfishSimPlatformAction};
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool as _};
    use carbide_secrets::test_support::credentials::TestCredentialManager;

    use super::*;

    struct UnavailableStore;

    #[async_trait]
    impl CredentialReader for UnavailableStore {
        async fn get_credentials(
            &self,
            _key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            Err(SecretsError::UfmCredentialReadBlocked {
                fabric: "store unavailable".to_string(),
            })
        }
    }

    /// The factory default is read only once the DPU's BIOS shows a UEFI
    /// password, so a DPU whose BIOS exposes none stages nothing even while the
    /// credential store is unavailable.
    #[tokio::test]
    async fn dpu_uefi_password_reads_the_factory_default_only_when_exposed() {
        struct Case {
            scenario: &'static str,
            exposed: bool,
            store_available: bool,
            expected: Result<bool, &'static str>,
        }
        let available = TestCredentialManager::default();
        let cases = [
            Case {
                scenario: "no UEFI password",
                exposed: false,
                store_available: false,
                expected: Ok(false),
            },
            Case {
                scenario: "store unavailable",
                exposed: true,
                store_available: false,
                expected: Err("factory default"),
            },
            Case {
                scenario: "staged",
                exposed: true,
                store_available: true,
                expected: Ok(true),
            },
        ];
        for Case {
            scenario,
            exposed,
            store_available,
            expected,
        } in cases
        {
            let reader: &dyn CredentialReader = if store_available {
                &available
            } else {
                &UnavailableStore
            };
            let sim = Arc::new(RedfishSim::default());
            if exposed {
                sim.set_bios_attributes(HashMap::from([(
                    "Attributes".to_string(),
                    serde_json::json!({ "CurrentUefiPassword": "" }),
                )]));
            }
            let bmc = BackendBmc::from(
                sim.create_client("127.0.0.1", None, RedfishAuth::Anonymous, None)
                    .await
                    .expect("the sim builds a client"),
            );

            let result =
                set_dpu_uefi_password(&bmc, reader, "site", &Instrumentation::noop()).await;

            match expected {
                Ok(staged) => assert_eq!(result.expect(scenario), staged, "{scenario}"),
                Err(reason) => assert!(
                    result.expect_err(scenario).to_string().contains(reason),
                    "{scenario}"
                ),
            }
            let changed = sim.platform_actions().iter().any(|action| {
                matches!(action, RedfishSimPlatformAction::ChangeUefiPassword { .. })
            });
            assert_eq!(changed, expected == Ok(true), "{scenario}");
        }
    }
}
