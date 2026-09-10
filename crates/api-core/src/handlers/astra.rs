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

use std::str::FromStr;

use ::rpc::forge::{AstraAttachment, AstraConfig, AstraConfigStatus, AstraPhase};
use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::spx::NULL_SPX_PARTITION_ID;
use config_version::ConfigVersion;
use db::ObjectColumnFilter;
use mac_address::MacAddress;
use model::dpa_interface::DpaSearchConfig;
use model::instance::config::spx::SpxAttachmentType;
use model::machine::ManagedHostStateSnapshot;
use model::machine::spx::{MachineSpxAttachmentStatusObservation, MachineSpxStatusObservation};
use tonic::Status;

use crate::CarbideError;
use crate::api::Api;

// Code to handle Astra specific information.

/// The Forge DPU agent periodically gRPC calls Carbide to GetManagedHostNetworkConfig.
/// This routine is called as a part of processing that request to retrieve and send
/// any Astra NIC configruation information in that response.
/// We need to look at the host associated with the DPU, find the Astra NICs
/// in the host. If the machine is an instance, we need to return the configuration
/// specified in the instance. Otherwise, we need to specify that each NIC should
/// not be associated with any VNI. If the host state is WaitingForDpaToBeReady,
/// we need to return the configuration specified in the instance.
/// 1) Is dpa enabled in config? If not, return None.
/// 2) Does the host associated with the DPU have any Astra NICs? If not, return None.
/// 3) Is the host associated with the DPU an instance? If so, return asta_config based on spx_config in instance.
/// 4) Otherwise, return astra_config with VNI set to 0 for each NIC.
pub(super) async fn get_astra_config(
    api: &Api,
    snapshot: &ManagedHostStateSnapshot,
) -> Result<Option<AstraConfig>, Status> {
    if !api.runtime_config.is_ewethers_enabled() || !api.runtime_config.is_astra_enabled() {
        tracing::debug!(
            "DPA is not enabled or Astra is not enabled, skipping Astra config retrieval"
        );
        return Ok(None);
    }

    // Find all Astra NICs in the host.
    let search_config = DpaSearchConfig {
        only_svpc: false,
        only_astra: true,
    };

    let mut txn = api.txn_begin().await?;

    let dpa_interfaces =
        db::dpa_interface::find_by_machine_id(&mut txn, snapshot.host_snapshot.id, search_config)
            .await?;

    txn.commit().await?;

    if dpa_interfaces.is_empty() {
        tracing::debug!(
            machine_id = %snapshot.host_snapshot.id,
            "No Astra NICs found; skipping Astra config retrieval",
        );
        return Ok(None);
    }

    let mut astra_attachments = Vec::new();

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to begin transaction: {e}"),
        })?;

    let subnet_ip = api
        .runtime_config
        .get_dpa_subnet_ip()
        .map_err(CarbideError::from)?;
    let subnet_mask = api
        .runtime_config
        .get_dpa_subnet_mask()
        .map_err(CarbideError::from)?;

    for dpa_interface in dpa_interfaces {
        if !dpa_interface.use_admin_network() {
            let instance = snapshot.instance.as_ref();
            let Some(instance) = instance else {
                // If use_admin_network is false, we expect an instance to be associated with the host.
                tracing::error!(
                    dpa_interface_id = %dpa_interface.id,
                    "DPA interface is not associated with an instance",
                );
                continue;
            };

            // From the instance spxconfig, find the spx_attachments that match the dpa_interface.mac_address
            // If we do not find a match, just continue and process other DPA interface objects.
            let Some(spx_attachment) =
                instance
                    .config
                    .spxconfig
                    .spx_attachments
                    .iter()
                    .find(|attachment| {
                        attachment.mac_address == Some(dpa_interface.mac_address.to_string())
                    })
            else {
                tracing::info!(
                    mac_address = %dpa_interface.mac_address,
                    instance_id = %instance.id,
                    "SPX attachment was not found",
                );
                continue;
            };

            // Now that we have the spx_attachment, get the spx_partition_id and query the database to get the DPA VNI.
            let spx_partition_id = spx_attachment.spx_partition_id;
            let dpa_vni = db::spx_partition::find_by(
                txn.as_mut(),
                ObjectColumnFilter::One(db::spx_partition::IdColumn, &spx_partition_id),
            )
            .await?;
            if dpa_vni.is_empty() {
                tracing::error!(
                    %spx_partition_id,
                    "SPX partition is not found",
                );
                continue;
            }

            let dpa_vni = dpa_vni[0].vni.unwrap_or(0);
            if dpa_vni == 0 {
                tracing::error!(
                    %spx_partition_id,
                    "SPX partition has no DPA VNI",
                );
                continue;
            }

            // Now we can create the Astra attachment and add it to the Astra config.
            let astra_attachment = AstraAttachment {
                mac_address: dpa_interface.mac_address.to_string(),
                vni: dpa_vni as u32,
                subnet_ipv4: subnet_ip.to_string(),
                subnet_mask,
                attachment_type: Some(SpxAttachmentType::Physical as i32),
                virtual_function_id: None, // TODO: Add virtual function id if supported
                network_name: None,        // TODO: Add network name when VMAAS support is added
                revision: instance.spx_config_version.to_string(),
            };

            astra_attachments.push(astra_attachment);
        } else {
            let astra_attachment = AstraAttachment {
                mac_address: dpa_interface.mac_address.to_string(),
                vni: 0,
                subnet_ipv4: subnet_ip.to_string(),
                subnet_mask,
                attachment_type: None,
                virtual_function_id: None,
                network_name: None,
                revision: dpa_interface.network_config.version.to_string(),
            };

            astra_attachments.push(astra_attachment);
        }
    }

    txn.commit()
        .await
        .map_err(|e| CarbideError::internal(format!("failed to commit astra config read: {e}")))?;

    Ok(Some(AstraConfig { astra_attachments }))
}

/// Processes Astra config status reported by the DPU agent.
/// This function is called when the DPU agent reports the Astra config status.
/// We need to update the Astra observation in the database.
/// 1) Is dpa enabled in config? If not, return None.
/// 2) Does the host associated with the DPU have any Astra NICs? If not, just return
pub(super) async fn process_astra_config_status(
    api: &Api,
    dpu_machine_id: &DpuMachineId,
    astra_config_status: &AstraConfigStatus,
) -> Result<(), Status> {
    if !api.runtime_config.is_ewethers_enabled() || !api.runtime_config.is_astra_enabled() {
        tracing::info!(
            "DPA is not enabled or Astra is not enabled, skipping Astra config status processing"
        );
        return Ok(());
    }

    let mut txn = api.txn_begin().await?;

    // Get the machine snapshot given the dpu_machine_id
    let snapshot = db::managed_host::load_snapshot(&mut txn, dpu_machine_id, Default::default())
        .await?
        .ok_or(CarbideError::NotFoundError {
            kind: "machine",
            id: dpu_machine_id.to_string(),
        })?;

    // Find all Astra NICs in the host.
    let search_config = DpaSearchConfig {
        only_svpc: false,
        only_astra: true,
    };

    let dpa_interfaces =
        db::dpa_interface::find_by_machine_id(&mut txn, snapshot.host_snapshot.id, search_config)
            .await?;

    if dpa_interfaces.is_empty() {
        // This should not happen. How is the DPU reporting the Astra config status if there are no Astra NICs?
        tracing::info!("No Astra NICs found in the host, skipping Astra config status processing");
        return Ok(());
    }

    txn.commit().await?;

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to begin transaction: {e}"),
        })?;

    let mut machine_observations = Vec::new();

    // Update the Astra observation in the database.
    for obs in astra_config_status.astra_attachments_status.iter() {
        // Skip attachments that are missing status or not in READY/DELETING phase.
        // If an observation is in the PENDING phase, wait for it to become READY before
        // recording the observation. If an observation is in the DELETING phase, we still
        // need to record it until it is completely deleted and no observations are sent for it.
        let Some(AstraPhase::PhaseReady | AstraPhase::PhaseDeleting) = obs
            .status
            .as_ref()
            .and_then(|status| AstraPhase::try_from(status.phase).ok())
        else {
            tracing::info!(
                astra_status_observation = ?obs,
                "Astra status is not READY or DELETING, skipping Astra config status processing",
            );
            continue;
        };

        // From the ack received from the DPA, figure out the config version currently
        // known to the DPA. If the DPA went through a powercycle, its config might be
        // invalid and the parsing below will fail.
        let ncv = match ConfigVersion::from_str(&obs.revision) {
            Ok(ncv) => ncv,
            Err(e) => {
                tracing::error!(
                    astra_status_observation = ?obs,
                    error = ?e,
                    "Failed to parse Astra DPA acknowledgment config version",
                );
                ConfigVersion::invalid()
            }
        };

        // If vni is non-zero, then we are in a tenancy and the partition_id is not None.
        // We need to get the partition_id correponding to this vni from the database.
        let vni = obs.vni;

        let mut spx_partition_id = NULL_SPX_PARTITION_ID;

        if vni != 0 {
            let partition = match db::spx_partition::find_by(
                txn.as_mut(),
                ObjectColumnFilter::List(db::spx_partition::VniColumn, &[vni]),
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(
                        vni,
                        error = ?e,
                        "Failed to find SPX partition",
                    );
                    continue;
                }
            };

            if partition.len() != 1 {
                // Given a VNI, we expect exactly one partition to be found.
                tracing::error!(
                    vni,
                    spx_partition_count = partition.len(),
                    "Unexpected number of SPX partitions found",
                );
                continue;
            }

            let spx_partition = &partition[0];
            spx_partition_id = spx_partition.id;

            tracing::debug!(
                vni,
                spx_partition = ?spx_partition,
                "Found SPX partition",
            );
        } else {
            tracing::debug!(
                astra_status_observation = ?obs,
                "Received VNI zero in Astra status observation",
            );
        }

        // Iterate through the dpa_interfaces and find the one that matches the mac_address in the current obs
        // Create local variable dpa_interface based on the matching mac_address found. If no match is found, continue to the next obs.
        let Ok(obs_mac) = MacAddress::from_str(&obs.mac_address) else {
            tracing::error!(mac_address = %obs.mac_address, "failed to parse MAC from Astra observation, skipping");
            continue;
        };
        let Some(dpa_interface) = dpa_interfaces
            .iter()
            .find(|dpa_interface| dpa_interface.mac_address == obs_mac)
        else {
            tracing::info!(
                mac_address = %obs.mac_address,
                machine_id = %snapshot.host_snapshot.id,
                "DPA interface was not found",
            );
            continue;
        };

        // Create the MachineSpxAttachmentStatusObservation based on the dpa_interface and obs
        let machine_observation = MachineSpxAttachmentStatusObservation {
            mac_address: dpa_interface.mac_address,
            partition_id: Some(spx_partition_id),
            config_version: Some(ncv),
            attachment_type: Some(SpxAttachmentType::Physical),
            virtual_function_id: None,
            observed_at: chrono::Utc::now(),
        };

        machine_observations.push(machine_observation);
    }

    let machine_observation = MachineSpxStatusObservation {
        spx_attachments: machine_observations,
        observed_at: chrono::Utc::now(),
    };

    db::machine::update_spx_status_observation(
        &mut txn,
        &snapshot.host_snapshot.id,
        &machine_observation,
    )
    .await?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {e}"),
    })?;

    Ok(())
}
