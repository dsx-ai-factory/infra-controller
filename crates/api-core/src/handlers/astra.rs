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

use ::rpc::forge::{
    AstraAttachment, AstraAttachmentOvs, AstraAttachmentVf, AstraConfig, AstraConfigStatus,
    AstraMultiplaneInterface, AstraPhase,
};
use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::spx::NULL_SPX_PARTITION_ID;
use config_version::ConfigVersion;
use db::ObjectColumnFilter;
use mac_address::MacAddress;
use model::dpa_interface::{DpaInterfaceType, DpaSearchConfig};
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
/// 1) Both Ewethers and Astra must be enabled in the runtime config; otherwise, return None.
/// 2) Every BF4 DPU has the host's declared CX9 logical ports as part in the expected machines db.
///    Return None only when both declared ports and discovered Astra NICs are absent.
/// 3) Is the host associated with the DPU an instance? If so, return asta_config based on spx_config in instance.
/// 4) Otherwise, return astra_config with VNI set to 0 for each NIC.
pub(super) async fn get_astra_config(
    api: &Api,
    snapshot: &ManagedHostStateSnapshot,
    dpu_machine_id: DpuMachineId,
) -> Result<Option<AstraConfig>, Status> {
    if !api.runtime_config.is_ewethers_enabled() || !api.runtime_config.is_astra_enabled() {
        tracing::debug!(
            "spx-maintenance: [controller] DPA is not enabled or Astra is not enabled, skipping Astra config retrieval"
        );
        return Ok(None);
    }

    // Find all Astra NICs in the host.
    let search_config = DpaSearchConfig {
        only_svpc: false,
        only_astra: true,
    };

    let mut txn = api.txn_begin().await?;

    // Include all declared ports on every config poll, even when their revisions
    // are unchanged. Unlike Weave, Xplane does not store or return our revision.
    // The agent reads each port's actual state and skips updates when it matches
    // the configured mode, detecting drift even without a configuration change.
    // An unset configured mode means observe only; the agent must not change it.
    let is_bf4_dpu = snapshot
        .dpu_snapshots
        .iter()
        .find(|dpu| dpu.id == dpu_machine_id)
        .is_some_and(super::spx_multiplane::is_bf4_dpu);
    let multiplane_interfaces = if is_bf4_dpu && api.runtime_config.is_partial_multiplane_enabled()
    {
        let configuration = match super::spx_multiplane::load_configuration(
            &mut txn,
            snapshot.host_snapshot.id,
        )
        .await
        {
            Ok(configuration) => configuration,
            Err(error @ CarbideError::InvalidArgument(_)) => {
                tracing::warn!(
                    machine_id = %snapshot.host_snapshot.id,
                    %dpu_machine_id,
                    error = %error,
                    "spx-maintenance: [controller] Omitting maintenance configuration from DPU poll: invalid multiplane declaration",
                );
                None
            }
            Err(error) => return Err(error.into()),
        };
        configuration
            .map(|configuration| {
                configuration
                    .interfaces
                    .into_iter()
                    .map(|(logical_ifname, state)| AstraMultiplaneInterface {
                        logical_ifname,
                        cfg_maintenance_mode: state.cfg_maintenance_mode,
                        reference: state.reference,
                        revision: state.revision.to_string(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let dpa_interfaces =
        db::dpa_interface::find_by_machine_id(&mut txn, snapshot.host_snapshot.id, search_config)
            .await?;

    txn.commit().await?;

    let is_attachment_config_present = !dpa_interfaces.is_empty();
    if !is_attachment_config_present && multiplane_interfaces.is_empty() {
        tracing::info!(
            machine_id = %snapshot.host_snapshot.id,
            %dpu_machine_id,
            "spx-maintenance: [controller] Not sending Astra config: no attachment configuration or eligible BF4 Astra maintenance configuration",
        );
        return Ok(None);
    }

    let mut astra_attachments = Vec::new();

    if is_attachment_config_present {
        let mut txn =
            api.database_connection
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
                    mac_address: dpa_interface.mac_address.to_string().to_lowercase(),
                    vni: dpa_vni as u32,
                    subnet_ipv4: subnet_ip.to_string(),
                    subnet_mask,
                    attachment_type: Some(spx_attachment.attachment_type.clone() as i32),
                    revision: instance.spx_config_version.to_string(),
                    attachment_vf: spx_attachment.attachment_vf.as_ref().map(|vf| {
                        AstraAttachmentVf {
                            vf_index: vf.vf_index,
                        }
                    }),
                    attachment_ovs: spx_attachment.attachment_ovs.as_ref().map(|ovs| {
                        AstraAttachmentOvs {
                            bridge_name: ovs.bridge_name.clone(),
                            network_name: ovs.ovn_network_name.clone(),
                        }
                    }),
                };

                astra_attachments.push(astra_attachment);
            } else {
                let astra_attachment = AstraAttachment {
                    mac_address: dpa_interface.mac_address.to_string().to_lowercase(),
                    vni: 0,
                    subnet_ipv4: subnet_ip.to_string(),
                    subnet_mask,
                    attachment_type: None,
                    revision: dpa_interface.network_config.version.to_string(),
                    attachment_vf: None,
                    attachment_ovs: None,
                };

                astra_attachments.push(astra_attachment);
            }
        }

        txn.commit().await.map_err(|e| {
            CarbideError::internal(format!("failed to commit astra config read: {e}"))
        })?;
    }

    tracing::info!(
        machine_id = %snapshot.host_snapshot.id,
        %dpu_machine_id,
        is_attachment_config_present,
        attachment_count = astra_attachments.len(),
        is_maintenance_config_present = !multiplane_interfaces.is_empty(),
        maintenance_interface_count = multiplane_interfaces.len(),
        "spx-maintenance: [controller] Sending Astra config to DPU",
    );

    Ok(Some(AstraConfig {
        astra_attachments,
        is_maintenance_config_present: Some(!multiplane_interfaces.is_empty()),
        multiplane_interfaces,
        is_attachment_config_present: Some(is_attachment_config_present),
    }))
}

/// `process_astra_config_status` replaces observations when Astra reporting is
/// enabled. Successful processing accepts Ready entries for current Astra
/// interfaces and preserves current SVPC observations. It drops observations
/// for absent interfaces; an empty report also clears all Astra readiness.
pub(super) async fn process_astra_config_status(
    api: &Api,
    dpu_machine_id: &DpuMachineId,
    astra_config_status: &AstraConfigStatus,
) -> Result<(), Status> {
    if !api.runtime_config.is_ewethers_enabled() || !api.runtime_config.is_astra_enabled() {
        tracing::info!(
            "spx-maintenance: [controller] DPA is not enabled or Astra is not enabled, skipping Astra config status processing"
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

    let is_maintenance_status_processed = astra_config_status
        .is_maintenance_status_present
        .unwrap_or(true)
        && api.runtime_config.is_partial_multiplane_enabled()
        && snapshot
            .dpu_snapshots
            .iter()
            .find(|dpu| dpu.id == *dpu_machine_id)
            .is_some_and(super::spx_multiplane::is_bf4_dpu);
    if is_maintenance_status_processed {
        super::spx_multiplane::record_observations(
            &mut txn,
            snapshot.host_snapshot.id,
            *dpu_machine_id,
            &astra_config_status.multiplane_interfaces_status,
        )
        .await?;
    }
    txn.commit().await?;
    tracing::info!(%dpu_machine_id, machine_id = %snapshot.host_snapshot.id, reported_interface_count = astra_config_status.multiplane_interfaces_status.len(), is_attachment_status_present = ?astra_config_status.is_attachment_status_present, is_maintenance_status_present = ?astra_config_status.is_maintenance_status_present, is_maintenance_status_processed, "spx-maintenance: [controller] Committed Astra status processing");

    if astra_config_status.is_attachment_status_present == Some(false) {
        return Ok(());
    }

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to begin transaction: {e}"),
        })?;

    let mut reported_observations = Vec::new();

    // Resolve reported VNIs before taking the host lock.
    for obs in astra_config_status.astra_attachments_status.iter() {
        let Some(AstraPhase::PhaseReady) = obs
            .status
            .as_ref()
            .and_then(|status| AstraPhase::try_from(status.phase).ok())
        else {
            tracing::info!(
                astra_status_observation = ?obs,
                "Astra status is not READY, skipping Astra observation",
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

        let Ok(obs_mac) = MacAddress::from_str(&obs.mac_address) else {
            tracing::error!(mac_address = %obs.mac_address, "failed to parse MAC from Astra observation, skipping");
            continue;
        };

        let machine_observation = MachineSpxAttachmentStatusObservation {
            mac_address: obs_mac,
            partition_id: Some(spx_partition_id),
            config_version: Some(ncv),
            attachment_type: Some(SpxAttachmentType::Physical),
            virtual_function_id: None,
            observed_at: chrono::Utc::now(),
        };

        reported_observations.push(machine_observation);
    }

    // Astra and SVPC share `spx_status_observation`. Read after taking the
    // host lock so a report committed while we waited is preserved.
    db::machine::lock_by_id_for_update(&mut txn, &snapshot.host_snapshot.id.into()).await?;
    let machine =
        db::machine::find_one(txn.as_mut(), &snapshot.host_snapshot.id, Default::default())
            .await?
            .ok_or(CarbideError::NotFoundError {
                kind: "machine",
                id: snapshot.host_snapshot.id.to_string(),
            })?;
    let dpa_interfaces = db::dpa_interface::find_by_machine_id(
        txn.as_mut(),
        snapshot.host_snapshot.id,
        DpaSearchConfig::default(),
    )
    .await?;
    let mut spx_attachment_observations = machine
        .status
        .spx_status_observation
        .unwrap_or_default()
        .spx_attachments;
    // A missing Astra interface does not make its old observation SVPC.
    // Classify both stored and reported entries from the current inventory.
    spx_attachment_observations.retain(|obs| {
        dpa_interfaces.iter().any(|interface| {
            interface.interface_type == DpaInterfaceType::Svpc
                && interface.mac_address == obs.mac_address
        })
    });
    reported_observations.retain(|obs| {
        dpa_interfaces.iter().any(|interface| {
            interface.interface_type == DpaInterfaceType::Astra
                && interface.mac_address == obs.mac_address
        })
    });
    spx_attachment_observations.extend(reported_observations);

    let spx_status_observation = MachineSpxStatusObservation {
        spx_attachments: spx_attachment_observations,
        observed_at: chrono::Utc::now(),
    };

    if let db::ConditionalWrite::NotApplied(reason) = db::machine::update_spx_status_observation(
        &mut txn,
        &snapshot.host_snapshot.id,
        &spx_status_observation,
    )
    .await?
    {
        return Err(db::DatabaseError::from(reason).into());
    }

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {e}"),
    })?;

    Ok(())
}
