// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Switch TLS setup after password rotation, before firmware work and ingestion.

use std::net::IpAddr;

use component_manager::error::ComponentManagerError;
use component_manager::nv_switch_manager::SwitchMtlsDisableState;
use component_manager::switch_endpoint::resolve_pre_ingestion_switch_endpoints;
use db::TransactionVending;
use model::component_manager::ConfigureSwitchCertificateState;
use model::site_explorer::{ExploredEndpoint, PreingestionState, SwitchCertificateRotationState};

use super::PreingestionManagerStatic;
use crate::errors::{PreingestionManagerError, PreingestionManagerResult};

impl PreingestionManagerStatic {
    pub(super) async fn rotate_switch_certificates(
        &self,
        endpoint: &ExploredEndpoint,
        state: &SwitchCertificateRotationState,
    ) -> PreingestionManagerResult<()> {
        use SwitchCertificateRotationState::*;

        if !endpoint.report.is_switch() {
            return Err(PreingestionManagerError::internal(
                "switch certificate preingestion requires a switch endpoint".to_string(),
            ));
        }
        let Some(interface) =
            db::machine_interface::find_by_ip(&self.database_connection, endpoint.address).await?
        else {
            return Err(PreingestionManagerError::internal(
                "switch BMC interface is unavailable for certificate rotation".to_string(),
            ));
        };
        let bmc_mac = interface.mac_address;
        if let Some(switch_id) =
            db::switch::find_switch_id_by_bmc_mac(&self.database_connection, bmc_mac).await?
        {
            return Err(PreingestionManagerError::internal(format!(
                "switch {switch_id} already exists for BMC {bmc_mac}; certificate preingestion must finish before switch creation",
            )));
        }

        let dependencies = self.component_manager.as_ref().ok_or_else(|| {
            PreingestionManagerError::internal(
                "switch certificate rotation dependencies are unavailable".to_string(),
            )
        })?;
        let manager = &dependencies.component_manager;

        match state {
            DisableMtls { job_id: None } | ConfigureCertificate { job_id: None } => {
                let expected = db::expected_switch::find_by_bmc_mac_address(
                    &self.database_connection,
                    bmc_mac,
                )
                .await?
                .ok_or_else(|| {
                    PreingestionManagerError::internal(format!(
                        "expected switch is unavailable for BMC {bmc_mac}",
                    ))
                })?;
                if expected.rack_id.is_none() {
                    return Err(PreingestionManagerError::internal(format!(
                        "switch certificate rotation requires a rack association for BMC {bmc_mac}",
                    )));
                }
                if dependencies.switch_mtls_services.is_empty() {
                    return Err(PreingestionManagerError::internal(format!(
                        "switch certificate rotation has no applicable mTLS services for BMC {bmc_mac}",
                    )));
                }

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
                        "switch endpoint resolution returned no result for BMC {bmc_mac}",
                    ))
                })?
                .endpoint?;

                // Without a saved job ID, restart may submit the operation again.
                let submission = match state {
                    DisableMtls { .. } => {
                        manager
                            .batch_disable_switch_mtls(
                                std::slice::from_ref(&switch_endpoint),
                                &dependencies.switch_mtls_services,
                            )
                            .await
                    }
                    _ => {
                        manager
                            .configure_switch_certificate(
                                &switch_endpoint,
                                None,
                                Some(&dependencies.switch_mtls_services),
                            )
                            .await
                    }
                };
                match submission {
                    Ok(job_id) if !job_id.trim().is_empty() => {
                        let rotation_state = match state {
                            DisableMtls { .. } => DisableMtls {
                                job_id: Some(job_id),
                            },
                            _ => ConfigureCertificate {
                                job_id: Some(job_id),
                            },
                        };
                        self.set_switch_certificate_state(
                            endpoint.address,
                            PreingestionState::SwitchCertificateRotation { rotation_state },
                        )
                        .await
                    }
                    outcome => {
                        let error = match outcome {
                            Ok(_) => ComponentManagerError::OperationOutcomeUnknown(
                                "switch TLS submission returned an empty job ID".to_string(),
                            ),
                            Err(error) => error,
                        };
                        self.set_switch_certificate_state(
                            endpoint.address,
                            PreingestionState::Failed {
                                reason: format!("switch TLS submission failed: {error}"),
                            },
                        )
                        .await?;
                        Err(error.into())
                    }
                }
            }
            DisableMtls {
                job_id: Some(job_id),
            } => {
                let status = manager.get_disable_switch_mtls_job_status(job_id).await?;
                match status.state {
                    SwitchMtlsDisableState::Pending => {
                        tracing::info!(
                            bmc_mac_address = %bmc_mac,
                            bmc_ip_address = %endpoint.address,
                            %job_id,
                            job_state = ?status.state,
                            "Waiting for switch mTLS removal"
                        );
                        Ok(())
                    }
                    SwitchMtlsDisableState::Completed => {
                        self.set_switch_certificate_state(
                            endpoint.address,
                            PreingestionState::SwitchCertificateRotation {
                                rotation_state: ConfigureCertificate { job_id: None },
                            },
                        )
                        .await
                    }
                    SwitchMtlsDisableState::Failed => {
                        self.set_switch_certificate_state(
                            endpoint.address,
                            PreingestionState::Failed {
                                reason: format!(
                                    "switch mTLS removal failed: {}",
                                    status.error.unwrap_or_else(|| job_id.clone())
                                ),
                            },
                        )
                        .await
                    }
                }
            }
            ConfigureCertificate {
                job_id: Some(job_id),
            } => {
                let status = manager
                    .get_configure_switch_certificate_job_status(job_id)
                    .await?;
                match status.state {
                    ConfigureSwitchCertificateState::Started
                    | ConfigureSwitchCertificateState::InProgress => {
                        tracing::info!(
                            bmc_mac_address = %bmc_mac,
                            bmc_ip_address = %endpoint.address,
                            %job_id,
                            job_state = ?status.state,
                            "Waiting for switch certificate installation"
                        );
                        Ok(())
                    }
                    ConfigureSwitchCertificateState::Completed => {
                        self.set_switch_certificate_state(
                            endpoint.address,
                            PreingestionState::CheckFirmware,
                        )
                        .await
                    }
                    ConfigureSwitchCertificateState::Failed => {
                        self.set_switch_certificate_state(
                            endpoint.address,
                            PreingestionState::Failed {
                                reason: format!(
                                    "switch certificate installation failed: {}",
                                    status.error.unwrap_or_else(|| job_id.clone())
                                ),
                            },
                        )
                        .await
                    }
                }
            }
        }
    }

    async fn set_switch_certificate_state(
        &self,
        address: IpAddr,
        state: PreingestionState,
    ) -> PreingestionManagerResult<()> {
        let mut txn = self.txn_begin().await?;
        db::explored_endpoints::set_preingestion_for_addresses(&[address], state, &mut txn).await?;
        txn.commit().await?;
        Ok(())
    }
}
