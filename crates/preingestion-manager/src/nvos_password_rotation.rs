// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Initial NVOS password rotation before switch ingestion and mTLS setup.

use std::net::IpAddr;

use carbide_secrets::credentials::{CredentialKey, Credentials};
use component_manager::component_common::RackComponent;
use component_manager::credentials::resolve_credentials;
use component_manager::nv_switch_manager::SwitchPasswordRotationState;
use component_manager::switch_endpoint::resolve_pre_ingestion_switch_endpoints;
use db::TransactionVending;
use db::credential_rotation::CredentialRotationType;
use mac_address::MacAddress;
use model::site_explorer::{ExploredEndpoint, NvosPasswordRotationState, PreingestionState};

#[cfg(test)]
mod tests;

use super::{ComponentManager, PreingestionManagerStatic};
use crate::errors::{PreingestionManagerError, PreingestionManagerResult};

impl PreingestionManagerStatic {
    /// Route switches to NVOS password handling after BMC time synchronization.
    /// Other endpoints continue through the existing firmware checks.
    pub(super) async fn handle_transition_from_bmc_time_sync(
        &self,
        endpoint: &ExploredEndpoint,
        repeat_firmware_check: bool,
    ) -> PreingestionManagerResult<bool> {
        if endpoint.report.is_switch() {
            self.set_nvos_state(
                endpoint.address,
                PreingestionState::NvosPasswordRotation {
                    rotation_state: NvosPasswordRotationState::Check,
                },
            )
            .await?;
            Ok(false)
        } else {
            self.check_firmware_versions_below_preingestion(endpoint, repeat_firmware_check)
                .await
        }
    }

    pub(super) async fn rotate_switch_nvos_password(
        &self,
        endpoint: &ExploredEndpoint,
        state: &NvosPasswordRotationState,
    ) -> PreingestionManagerResult<()> {
        let Some(interface) =
            db::machine_interface::find_by_ip(&self.database_connection, endpoint.address).await?
        else {
            tracing::info!(bmc_ip_address = %endpoint.address, "Waiting for switch BMC interface");
            return Ok(());
        };
        // DHCP-discovered switch BMCs remain typed Data until switch creation.
        // The exploration report identifies this endpoint as a switch BMC.
        let bmc_mac = interface.mac_address;
        if let Some(switch_id) =
            db::switch::find_switch_id_by_bmc_mac(&self.database_connection, bmc_mac).await?
        {
            return Err(PreingestionManagerError::internal(format!(
                "Switch {switch_id} already exists for BMC {bmc_mac}; NVOS password preingestion must finish before switch creation",
            )));
        }
        let Some(dependencies) = &self.component_manager else {
            return Err(PreingestionManagerError::internal(
                "NVOS password rotation dependencies are unavailable".to_string(),
            ));
        };
        if !dependencies
            .component_manager
            .nv_switch
            .supports_password_rotation()
        {
            return Err(PreingestionManagerError::internal(
                "NVOS password rotation is not supported by the configured backend".to_string(),
            ));
        }

        match state {
            NvosPasswordRotationState::Check => {
                self.claim_nvos_password_rotation(endpoint.address, bmc_mac)
                    .await
            }
            NvosPasswordRotationState::Rotate {
                target_version,
                job_id: None,
            } => {
                let switch_endpoint = resolve_pre_ingestion_switch_endpoints(
                    &self.database_connection,
                    dependencies.credential_manager.as_ref(),
                    &[bmc_mac],
                    None,
                )
                .await?
                .pop()
                .ok_or_else(|| {
                    PreingestionManagerError::internal(format!(
                        "Switch endpoint resolution returned no result for BMC {bmc_mac}",
                    ))
                })?
                .endpoint?;
                let Credentials::UsernamePassword { password, .. } = get_target_credentials(
                    dependencies,
                    *target_version,
                    &switch_endpoint.nvos_credentials,
                )
                .await?;
                // The target was persisted in Check. Retry this exact version
                // if dispatch or saving its returned job ID is interrupted.
                let job_id = dependencies
                    .component_manager
                    .ensure_switch_password_rotation(&switch_endpoint, &password)
                    .await?;
                if job_id.is_empty() {
                    return Err(PreingestionManagerError::internal(
                        "NVOS password submission returned an empty job ID".to_string(),
                    ));
                }
                self.set_nvos_rotation_state(endpoint.address, *target_version, Some(job_id))
                    .await
            }
            NvosPasswordRotationState::Rotate {
                target_version,
                job_id: Some(job_id),
            } => {
                let status = dependencies
                    .component_manager
                    .get_switch_password_rotation_job_status(job_id)
                    .await?;
                match status {
                    SwitchPasswordRotationState::Pending => Ok(()),
                    SwitchPasswordRotationState::Completed => {
                        self.complete_nvos_password_rotation(
                            endpoint.address,
                            bmc_mac,
                            *target_version,
                            dependencies,
                        )
                        .await
                    }
                    SwitchPasswordRotationState::Failed
                    | SwitchPasswordRotationState::NotFound
                    | SwitchPasswordRotationState::Unknown => {
                        tracing::info!(%bmc_mac, ?status, "Retrying captured NVOS password target");
                        self.set_nvos_rotation_state(endpoint.address, *target_version, None)
                            .await
                    }
                }
            }
        }
    }

    async fn complete_nvos_password_rotation(
        &self,
        address: IpAddr,
        bmc_mac: MacAddress,
        target_version: u32,
        dependencies: &ComponentManager,
    ) -> PreingestionManagerResult<()> {
        let key = CredentialKey::SwitchNvosAdmin {
            bmc_mac_address: bmc_mac,
        };
        let current = self
            .get_current_nvos_credentials(bmc_mac, dependencies)
            .await?;
        let target = get_target_credentials(dependencies, target_version, &current).await?;
        persist_credentials(dependencies, &key, &target).await?;
        let applied_version = i32::try_from(target_version).map_err(|_| {
            PreingestionManagerError::internal(
                "NVOS target version exceeds database range".to_string(),
            )
        })?;
        self.record_nvos_password_rotation_succeeded(address, bmc_mac, applied_version)
            .await
    }

    /// Use expected-switch credentials until a per-switch credential has been stored.
    async fn get_current_nvos_credentials(
        &self,
        bmc_mac: MacAddress,
        dependencies: &ComponentManager,
    ) -> PreingestionManagerResult<Credentials> {
        Ok(resolve_credentials(
            &self.database_connection,
            dependencies.credential_manager.as_ref(),
            RackComponent::SwitchNvos,
            bmc_mac,
            None,
        )
        .await?)
    }

    async fn set_nvos_rotation_state(
        &self,
        address: IpAddr,
        target_version: u32,
        job_id: Option<String>,
    ) -> PreingestionManagerResult<()> {
        self.set_nvos_state(
            address,
            PreingestionState::NvosPasswordRotation {
                rotation_state: NvosPasswordRotationState::Rotate {
                    target_version,
                    job_id,
                },
            },
        )
        .await
    }

    async fn set_nvos_state(
        &self,
        address: IpAddr,
        state: PreingestionState,
    ) -> PreingestionManagerResult<()> {
        let mut txn = self.txn_begin().await?;
        db::explored_endpoints::set_preingestion_for_addresses(&[address], state, &mut txn).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Claim rotation under the common interface lock, respecting aliases excluded
    /// from the current iteration by an exploration error or refresh wait.
    async fn claim_nvos_password_rotation(
        &self,
        address: IpAddr,
        bmc_mac: MacAddress,
    ) -> PreingestionManagerResult<()> {
        let mut txn = self.txn_begin().await?;
        let interfaces =
            db::machine_interface::find_by_mac_address_for_update(&mut txn, bmc_mac).await?;
        let addresses = interfaces
            .into_iter()
            .flat_map(|interface| interface.addresses)
            .collect();
        // Read aliases before the version: completion atomically publishes the
        // version and removes the Rotate owner, so a claimant sees at least one.
        let aliases = db::explored_endpoints::find_by_ips(&mut *txn, addresses).await?;
        if !aliases.iter().any(|alias| {
            alias.address == address
                && matches!(
                    alias.preingestion_state,
                    PreingestionState::NvosPasswordRotation {
                        rotation_state: NvosPasswordRotationState::Check,
                    }
                )
        }) {
            // A stale Check must not overwrite a claim or a later state.
            txn.commit().await?;
            return Ok(());
        }

        let operation = db::credential_rotation::device_rotation_operation_state(
            &mut *txn,
            CredentialRotationType::Nvos,
            bmc_mac,
        )
        .await?;
        if let Some(current_version) = operation.and_then(|operation| operation.current_version) {
            tracing::info!(
                %bmc_mac,
                bmc_ip_address = %address,
                current_version,
                "NVOS password already has a confirmed site version; continuing to firmware checks"
            );
            db::explored_endpoints::set_preingestion_for_addresses(
                &[address],
                PreingestionState::CheckFirmware,
                &mut txn,
            )
            .await?;
            txn.commit().await?;
            return Ok(());
        }

        if let Some(owner) = aliases.iter().find(|alias| {
            matches!(
                alias.preingestion_state,
                PreingestionState::NvosPasswordRotation {
                    rotation_state: NvosPasswordRotationState::Rotate { .. },
                }
            )
        }) {
            tracing::info!(
                %bmc_mac,
                bmc_ip_address = %address,
                owner_ip_address = %owner.address,
                "Waiting for the NVOS password rotation owner"
            );
            txn.commit().await?;
            return Ok(());
        }
        let Some(target_version) = db::credential_rotation::current_target_version(
            &mut *txn,
            CredentialRotationType::Nvos,
        )
        .await?
        else {
            return Err(PreingestionManagerError::internal(format!(
                "NVOS site password target is missing during preingestion for BMC {bmc_mac}",
            )));
        };
        let target_version = u32::try_from(target_version).map_err(|_| {
            PreingestionManagerError::internal("NVOS target version is negative".to_string())
        })?;

        // Capture the target with the claim; Rotate validates credentials before dispatch.
        db::explored_endpoints::set_preingestion_for_addresses(
            &[address],
            PreingestionState::NvosPasswordRotation {
                rotation_state: NvosPasswordRotationState::Rotate {
                    target_version,
                    job_id: None,
                },
            },
            &mut txn,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn record_nvos_password_rotation_succeeded(
        &self,
        address: IpAddr,
        bmc_mac: MacAddress,
        applied_version: i32,
    ) -> PreingestionManagerResult<()> {
        let mut txn = self.txn_begin().await?;
        db::credential_rotation::record_nvos_preingestion_rotation_succeeded(
            &mut txn,
            bmc_mac,
            applied_version,
        )
        .await?;
        db::explored_endpoints::set_preingestion_for_addresses(
            &[address],
            PreingestionState::CheckFirmware,
            &mut txn,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }
}

/// Resolve the captured site password while preserving the switch's username.
async fn get_target_credentials(
    dependencies: &ComponentManager,
    target_version: u32,
    current: &Credentials,
) -> PreingestionManagerResult<Credentials> {
    let Credentials::UsernamePassword { username, password } = current;
    if username.is_empty() || password.is_empty() {
        return Err(PreingestionManagerError::internal(
            "current NVOS username and password must both be nonempty".to_string(),
        ));
    }
    let target = dependencies
        .credential_manager
        .get_credentials_from_writer(&CredentialKey::switch_nvos_site_admin(target_version))
        .await?
        .ok_or_else(|| {
            PreingestionManagerError::internal(format!(
                "NVOS target credential version {target_version} is missing",
            ))
        })?;
    let Credentials::UsernamePassword { password, .. } = target;
    if password.is_empty() {
        return Err(PreingestionManagerError::internal(
            "NVOS target password must not be empty".to_string(),
        ));
    }
    Ok(Credentials::new(username, password))
}

async fn persist_credentials(
    dependencies: &ComponentManager,
    key: &CredentialKey,
    credentials: &Credentials,
) -> PreingestionManagerResult<()> {
    dependencies
        .credential_manager
        .set_credentials(key, credentials)
        .await?;
    if dependencies
        .credential_manager
        .get_credentials_from_writer(key)
        .await?
        .as_ref()
        != Some(credentials)
        || dependencies
            .credential_manager
            .get_credentials(key)
            .await?
            .as_ref()
            != Some(credentials)
    {
        return Err(PreingestionManagerError::internal(
            "Stored NVOS credential does not match the configured writer and effective reader"
                .to_string(),
        ));
    }
    Ok(())
}
