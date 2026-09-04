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

//! Rack compute-tray firmware handling for preingestion.

use std::net::IpAddr;

use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use component_manager::compute_tray_manager::{
    ComputeTrayEndpoint, ComputeTrayManager, ComputeTrayVendor,
};
use component_manager::error::ComponentManagerError;
use component_manager::types::FirmwareUpdateOptions;
use db::{DatabaseError, WithTransaction};
use futures_util::FutureExt;
use model::component_manager::FirmwareState;
use model::machine_interface::InterfaceType;
use model::rack_type::RackFirmwareObjectConfig;
use model::site_explorer::{ExploredEndpoint, PreingestionState};
use sqlx::PgPool;

use super::{PreingestionManagerStatic, RackFirmwareDependencies};
use crate::errors::PreingestionManagerResult;

struct PreparedRackFirmware {
    endpoint: ComputeTrayEndpoint,
    config_json: String,
    access_token: Option<String>,
}

impl PreingestionManagerStatic {
    /// Routes rack compute trays through RMS before the standalone host path.
    ///
    /// RMS receives the SOT unchanged and owns inventory/version decisions.
    /// `true` means the rack workflow owns this endpoint, including retries,
    /// no-op completion, and terminal failures.
    pub(super) async fn start_rack_firmware(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> PreingestionManagerResult<bool> {
        let Some(dependencies) = self.rack_firmware.as_ref() else {
            return Ok(false);
        };

        let Some((bmc_mac, aliases)) = Self::rack_firmware_identity(db, endpoint).await? else {
            return Ok(false);
        };

        let addresses: Vec<IpAddr> = aliases.iter().map(|alias| alias.address).collect();

        // A healthy sibling can poll the physical tray when the endpoint that
        // entered the wait state is temporarily excluded from this iteration.
        if aliases.iter().any(|alias| {
            matches!(
                &alias.preingestion_state,
                PreingestionState::RackFirmwareUpdateWait { .. }
            )
        }) {
            self.wait_for_rack_firmware(db, endpoint).await?;
            return Ok(true);
        }

        let Some(identity) = db::expected_machine::find_rms_identities_by_bmc_macs(db, &[bmc_mac])
            .await?
            .pop()
        else {
            return Ok(false);
        };

        // Do not replace a sibling alias state while standalone firmware work
        // is already changing the same physical BMC.
        if aliases.iter().any(|alias| {
            alias.address != endpoint.address
                && matches!(
                    &alias.preingestion_state,
                    PreingestionState::InitialReset { .. }
                        | PreingestionState::UpgradeFirmwareWait { .. }
                        | PreingestionState::ResetForNewFirmware { .. }
                        | PreingestionState::NewFirmwareReportedWait { .. }
                        | PreingestionState::ScriptRunning
                )
        }) {
            return Ok(true);
        }

        // Only one IP alias submits work for the physical BMC.
        let owner = aliases
            .iter()
            .filter(|alias| {
                !matches!(
                    &alias.preingestion_state,
                    PreingestionState::Complete | PreingestionState::Failed { .. }
                ) && !alias.waiting_for_explorer_refresh
                    && alias.report.last_exploration_error.is_none()
            })
            .map(|alias| alias.address)
            .min();

        if owner != Some(endpoint.address) {
            return Ok(true);
        }

        let Some(profile_id) = identity.rack_profile_id else {
            self.fail_rack_firmware(
                db,
                &addresses,
                format!(
                    "expected rack compute tray {bmc_mac} in rack {} has no rack profile",
                    identity.rack_id
                ),
            )
            .await?;

            return Ok(true);
        };

        let Some(profile) = dependencies.rack_profiles.get(profile_id.as_str()) else {
            self.fail_rack_firmware(
                db,
                &addresses,
                format!("rack profile {profile_id} is not configured"),
            )
            .await?;

            return Ok(true);
        };

        let Some(firmware_object) = profile.firmware_object.clone() else {
            tracing::debug!(
                rack_profile_id = %profile_id,
                %bmc_mac,
                "Rack profile has no firmware object; skipping automatic firmware update"
            );

            self.set_rack_firmware_state(db, &addresses, PreingestionState::Complete)
                .await?;

            return Ok(true);
        };

        let preparation = self
            .prepare_rack_firmware(endpoint, bmc_mac, &firmware_object, dependencies)
            .await;

        let preparation = match preparation {
            Ok(Some(preparation)) => preparation,
            Ok(None) => return Ok(true),
            Err(reason) => {
                self.fail_rack_firmware(db, &addresses, reason).await?;

                return Ok(true);
            }
        };

        self.submit_rack_firmware(
            db,
            &addresses,
            dependencies.compute_tray.as_ref(),
            preparation,
        )
        .await?;

        Ok(true)
    }

    async fn submit_rack_firmware(
        &self,
        db: &PgPool,
        addresses: &[IpAddr],
        compute_tray: &dyn ComputeTrayManager,
        preparation: PreparedRackFirmware,
    ) -> PreingestionManagerResult<()> {
        self.set_rack_firmware_state(
            db,
            addresses,
            PreingestionState::RackFirmwareUpdateWait {
                backend_job_id: None,
            },
        )
        .await?;

        let PreparedRackFirmware {
            endpoint,
            config_json,
            access_token,
        } = preparation;

        let results = compute_tray
            .update_firmware(
                &[endpoint],
                &config_json,
                &[],
                &FirmwareUpdateOptions {
                    access_token,
                    force_update: false,
                },
            )
            .await;

        let result = match results {
            Ok(results) => match results.as_slice() {
                [result] => result.clone(),
                _ => {
                    self.fail_rack_firmware(
                        db,
                        addresses,
                        format!(
                            "RMS returned {} results for one rack compute tray",
                            results.len()
                        ),
                    )
                    .await?;

                    return Ok(());
                }
            },
            Err(
                ComponentManagerError::Internal(error)
                | ComponentManagerError::RejectedBeforeDispatch(error),
            ) => {
                tracing::warn!(
                    %error,
                    "RMS firmware submission did not reach the backend; will retry"
                );

                self.set_rack_firmware_state(db, addresses, PreingestionState::RecheckVersions)
                    .await?;

                return Ok(());
            }
            Err(error) => {
                self.fail_rack_firmware(
                    db,
                    addresses,
                    format!("rack firmware submission outcome is unknown: {error}"),
                )
                .await?;

                return Ok(());
            }
        };

        if !result.success {
            let mut reason = result
                .error
                .unwrap_or_else(|| "RMS rejected the rack firmware update".to_string());

            // A job ID does not override a failed RMS node or batch result.
            // Preserve it in the diagnostic so the operator can reconcile the
            // backend without risking a duplicate submission.
            if let Some(backend_job_id) = result.backend_job_id {
                reason = format!(
                    "{reason}; RMS returned backend job ID {backend_job_id} with the failed response; reconcile the RMS job before retrying pre-ingestion"
                );
            }

            self.fail_rack_firmware(db, addresses, reason).await?;

            return Ok(());
        }

        if let Some(backend_job_id) = result.backend_job_id {
            self.set_rack_firmware_state(
                db,
                addresses,
                PreingestionState::RackFirmwareUpdateWait {
                    backend_job_id: Some(backend_job_id),
                },
            )
            .await?;

            return Ok(());
        }

        // RMS found no work to schedule, so ingestion can continue.
        self.set_rack_firmware_state(db, addresses, PreingestionState::Complete)
            .await?;

        Ok(())
    }

    async fn rack_firmware_identity(
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> Result<Option<(mac_address::MacAddress, Vec<ExploredEndpoint>)>, DatabaseError> {
        let Some(interface) = db::machine_interface::find_by_ip(db, endpoint.address).await? else {
            return Ok(None);
        };

        if interface.interface_type != InterfaceType::Bmc {
            return Ok(None);
        }

        let mut addresses = interface.addresses;
        addresses.push(endpoint.address);
        addresses.sort_unstable();
        addresses.dedup();

        let aliases = db::explored_endpoints::find_by_ips(db, addresses).await?;

        Ok(Some((interface.mac_address, aliases)))
    }

    async fn prepare_rack_firmware(
        &self,
        explored: &ExploredEndpoint,
        bmc_mac: mac_address::MacAddress,
        firmware_object: &RackFirmwareObjectConfig,
        dependencies: &RackFirmwareDependencies,
    ) -> Result<Option<PreparedRackFirmware>, String> {
        let Some(endpoint) = self.prepare_compute_endpoint(explored, bmc_mac).await? else {
            return Ok(None);
        };

        let reader = self.credential_reader.as_ref().ok_or_else(|| {
            "credential reader is not configured for rack firmware updates".to_string()
        })?;

        let access_token = match firmware_object.access_token_credential.as_ref() {
            None => None,
            Some(name) => {
                let key = CredentialKey::FirmwareArtifactAccessToken { name: name.clone() };

                match reader.get_credentials(&key).await {
                    Ok(Some(Credentials::UsernamePassword { password, .. })) => Some(password),
                    Ok(None) => {
                        tracing::warn!(
                            credential_name = %name,
                            "Firmware artifact access-token credential is unavailable; will retry"
                        );

                        return Ok(None);
                    }
                    Err(error) => {
                        tracing::warn!(
                            credential_name = %name,
                            %error,
                            "Firmware artifact access-token credential is unavailable; will retry"
                        );

                        return Ok(None);
                    }
                }
            }
        };

        let config_json = match dependencies
            .firmware_object_fetcher
            .fetch(firmware_object.url.as_str(), firmware_object.fetch_timeout)
            .await
        {
            Ok(config_json) => config_json,
            Err(error) => {
                tracing::warn!(
                    bmc_ip_address = %explored.address,
                    bmc_mac_address = %bmc_mac,
                    %error,
                    "Firmware object is unavailable; will retry"
                );

                return Ok(None);
            }
        };

        Ok(Some(PreparedRackFirmware {
            endpoint,
            config_json,
            access_token,
        }))
    }

    /// Polls the RMS job and applies its outcome to every BMC IP alias.
    pub(super) async fn wait_for_rack_firmware(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> PreingestionManagerResult<()> {
        let Some((bmc_mac, aliases)) = Self::rack_firmware_identity(db, endpoint).await? else {
            return self
                .fail_rack_firmware(
                    db,
                    &[endpoint.address],
                    "BMC identity is missing while polling an RMS firmware job".to_string(),
                )
                .await;
        };

        let addresses: Vec<IpAddr> = aliases.iter().map(|alias| alias.address).collect();

        let mut waiting_jobs = aliases
            .iter()
            .filter_map(|alias| match &alias.preingestion_state {
                PreingestionState::RackFirmwareUpdateWait { backend_job_id } => {
                    Some(backend_job_id)
                }
                _ => None,
            });

        let Some(backend_job_id) = waiting_jobs.next() else {
            return self
                .fail_rack_firmware(
                    db,
                    &addresses,
                    "RMS firmware wait state is missing for the rack compute tray".to_string(),
                )
                .await;
        };

        if waiting_jobs.any(|candidate| candidate != backend_job_id) {
            return self
                .fail_rack_firmware(
                    db,
                    &addresses,
                    "BMC aliases contain conflicting RMS firmware job IDs".to_string(),
                )
                .await;
        }

        let poll_owner = aliases
            .iter()
            .filter(|candidate| {
                matches!(
                    &candidate.preingestion_state,
                    PreingestionState::RackFirmwareUpdateWait { .. }
                ) && !candidate.waiting_for_explorer_refresh
                    && candidate.report.last_exploration_error.is_none()
            })
            .map(|candidate| candidate.address)
            .min();

        if poll_owner.is_some() && poll_owner != Some(endpoint.address) {
            return Ok(());
        }

        let Some(job_id) = backend_job_id.as_deref() else {
            return self
                .fail_rack_firmware(
                    db,
                    &addresses,
                    "RMS firmware submission outcome is ambiguous because no job ID was persisted; reconcile the tray before retrying"
                        .to_string(),
                )
                .await;
        };

        let Some(dependencies) = self.rack_firmware.as_ref() else {
            return self
                .fail_rack_firmware(
                    db,
                    &addresses,
                    "RMS compute-tray firmware support is no longer configured".to_string(),
                )
                .await;
        };

        let status = match dependencies
            .compute_tray
            .get_firmware_job_status(endpoint.address, bmc_mac, job_id)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    bmc_mac_address = %bmc_mac,
                    backend_job_id = %job_id,
                    %error,
                    "RMS firmware job status is unavailable; will retry"
                );

                return Ok(());
            }
        };

        match status.state {
            FirmwareState::Queued | FirmwareState::InProgress | FirmwareState::Verifying => Ok(()),
            FirmwareState::Unknown => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    bmc_mac_address = %bmc_mac,
                    backend_job_id = %job_id,
                    error = status.error.as_deref().unwrap_or("status unavailable"),
                    "RMS firmware job status is unavailable; will retry"
                );

                Ok(())
            }
            FirmwareState::Completed => {
                self.set_rack_firmware_state(db, &addresses, PreingestionState::Complete)
                    .await
            }
            FirmwareState::Failed | FirmwareState::Cancelled => {
                let reason = status.error.unwrap_or_else(|| {
                    format!("RMS firmware job ended in state {:?}", status.state)
                });

                self.fail_rack_firmware(db, &addresses, reason).await
            }
        }
    }

    async fn prepare_compute_endpoint(
        &self,
        explored: &ExploredEndpoint,
        bmc_mac: mac_address::MacAddress,
    ) -> Result<Option<ComputeTrayEndpoint>, String> {
        let Some(reader) = self.credential_reader.as_ref() else {
            return Err(
                "credential reader is not configured for rack firmware updates".to_string(),
            );
        };

        let key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: bmc_mac,
            },
        };

        match reader.get_credentials(&key).await {
            Ok(Some(credentials)) => Ok(Some(ComputeTrayEndpoint {
                vendor: ComputeTrayVendor::from(
                    explored
                        .report
                        .vendor
                        .unwrap_or(bmc_vendor::BMCVendor::Unknown),
                ),
                bmc_ip: explored.address,
                bmc_mac,
                bmc_credentials: credentials,
            })),
            Ok(None) => {
                tracing::warn!(%bmc_mac, "BMC credentials are unavailable; will retry");
                Ok(None)
            }
            Err(error) => {
                tracing::warn!(%bmc_mac, %error, "BMC credentials are unavailable; will retry");
                Ok(None)
            }
        }
    }

    async fn set_rack_firmware_state(
        &self,
        db: &PgPool,
        addresses: &[IpAddr],
        state: PreingestionState,
    ) -> PreingestionManagerResult<()> {
        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_for_addresses(addresses, state, txn.as_mut())
                .boxed()
        })
        .await??;

        Ok(())
    }

    async fn fail_rack_firmware(
        &self,
        db: &PgPool,
        addresses: &[IpAddr],
        reason: String,
    ) -> PreingestionManagerResult<()> {
        tracing::error!(bmc_ip_addresses = ?addresses, %reason, "Rack firmware preingestion failed");
        self.set_rack_firmware_state(db, addresses, PreingestionState::Failed { reason })
            .await
    }
}
