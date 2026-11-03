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

use std::collections::{HashMap, HashSet};

use ::rpc::forge::{
    AstraAttachmentStatus, AstraConfig, AstraConfigStatus, AstraPhase, AstraStatus,
    SpxAttachmentType,
};
use carbide_utils::none_if_empty::NoneIfEmpty;
use eyre::WrapErr;

use crate::weave_ew_vpc_client::proto::state::Phase;
use crate::weave_ew_vpc_client::proto::{
    AttachmentOvs, AttachmentPf, AttachmentType, AttachmentVf,
    CreateVirtualNetworkAttachmentRequest, CreateVirtualNetworkRequest,
    DeleteVirtualNetworkAttachmentRequest, DeleteVirtualNetworkRequest,
    ListVirtualNetworkAttachmentsRequest, ListVirtualNetworksRequest, ObjectMetadata, State,
    UpdateVirtualNetworkAttachmentRequest, UpdateVirtualNetworkRequest, VirtualNetwork,
    VirtualNetworkAttachment, VirtualNetworkAttachmentSpec, VirtualNetworkSpec,
};
use crate::weave_ew_vpc_client::{
    WEAVE_EW_VPC_FLOW_CONTROLLER_SOCKET_PATH, weave_ew_vpc_create_virtual_network,
    weave_ew_vpc_create_virtual_network_attachment, weave_ew_vpc_delete_virtual_network,
    weave_ew_vpc_delete_virtual_network_attachment, weave_ew_vpc_list_virtual_network_attachments,
    weave_ew_vpc_list_virtual_networks, weave_ew_vpc_update_virtual_network,
    weave_ew_vpc_update_virtual_network_attachment,
};

fn astra_weave_ew_vpc_virtual_network_id(vni: i32) -> String {
    format!("astra-weave-vni-{vni}")
}

const WEAVE_EW_VPC_REVISION_USER_DATA_KEY: &str = "revision";

fn weave_ew_vpc_object_metadata(id: Option<String>, revision: &str) -> ObjectMetadata {
    ObjectMetadata {
        id,
        resource_version: None,
        creation_timestamp: None,
        deletion_timestamp: None,
        user_data: HashMap::from([(
            WEAVE_EW_VPC_REVISION_USER_DATA_KEY.to_string(),
            revision.to_string(),
        )]),
    }
}

// Given an AstraAttachment sent from carbide, this routine builds and
// returns a VirtualNetworkAttachmentSpec that can be used to create a
// VirtualNetworkAttachment on the DOCA Weave server.
fn weave_ew_virtual_network_attachment_spec_from_astra_attachment(
    astra_attachment_status: &AstraAttachmentStatus,
) -> Result<VirtualNetworkAttachmentSpec, State> {
    let Some(astra_attachment_type) = astra_attachment_status.attachment_type else {
        return Err(State {
            phase: Phase::Error.into(),
            reason: "Missing Astra SpxAttachmentType".to_string(),
            message: "create_virtual_network_attachment".to_string(),
        });
    };

    let astra_attachment_type =
        SpxAttachmentType::try_from(astra_attachment_type).map_err(|_| State {
            phase: Phase::Error.into(),
            reason: "Unknown Astra SpxAttachmentType".to_string(),
            message: "create_virtual_network_attachment".to_string(),
        })?;

    let mut spec = VirtualNetworkAttachmentSpec {
        vnet_id: astra_weave_ew_vpc_virtual_network_id(astra_attachment_status.vni),
        nic_id: astra_attachment_status.mac_address.clone(),
        attachment_type: AttachmentType::Unspecified.into(),
        attachment_pf: None,
        attachment_vf: None,
        attachment_ovs: None,
    };

    match astra_attachment_type {
        SpxAttachmentType::Physical => {
            spec.attachment_type = AttachmentType::Pf.into();
            spec.attachment_pf = Some(AttachmentPf {
                pf_id: astra_attachment_status.mac_address.clone(),
            });
        }
        SpxAttachmentType::Virtual => {
            let Some(attachment_vf) = astra_attachment_status.attachment_vf.as_ref() else {
                return Err(State {
                    phase: Phase::Error.into(),
                    reason: "Missing Astra virtual_function_id".to_string(),
                    message: "create_virtual_network_attachment".to_string(),
                });
            };
            let vf_index = attachment_vf.vf_index;

            spec.attachment_type = AttachmentType::Vf.into();
            spec.attachment_vf = Some(AttachmentVf {
                pf_id: astra_attachment_status.mac_address.clone(),
                vf_index,
            });
        }
        SpxAttachmentType::Ovs => {
            spec.attachment_type = AttachmentType::Ovs.into();
            spec.attachment_ovs = Some(AttachmentOvs {
                // ovn_network_name is optional; carry through whatever the status
                // provides, or None if it is absent.
                ovn_network_name: astra_attachment_status
                    .attachment_ovs
                    .as_ref()
                    .and_then(|attachment_ovs| attachment_ovs.network_name.clone()),
                bridge_name: astra_attachment_status
                    .attachment_ovs
                    .as_ref()
                    .map(|attachment_ovs| attachment_ovs.bridge_name.clone())
                    .unwrap_or_default(),
            });
        }
    }

    Ok(spec)
}

// Take a diff of the Astra config vs the DOCA Weave
// server virtual networks. Notify to create new virtual networks that are
// missing on the server. Notify to update the revision string on the weave
// server if the virtual network exists. Deletion of virtual networks that
// are present on the weave server but not in the astra config is done after
// we process the astra attachments as they are needed there.
async fn create_update_ew_vpc_virtual_networks(
    socket_path: &str,
    astra_config_status: &mut AstraConfigStatus,
) -> eyre::Result<()> {
    // Get list of existing virtual networks from the Doca Weave server
    let list_vni_req = ListVirtualNetworksRequest { vni: None };
    let list_vni_rsp = weave_ew_vpc_list_virtual_networks(socket_path, list_vni_req).await?;

    // Log virtual networks on the weave server.
    log_virtual_networks(&list_vni_rsp.virtual_networks);

    // From the list of virtual networks on the DOCA Weave server (could be
    // empty), build a seen_virtual_networks_hashmap of (vni, (metadata,
    // state)) for each virtual network. If weave returns a virtual network
    // with invalid metadata or status, track it as an error to copy into the
    // astra attachment status.
    let mut seen_virtual_networks_hashmap: HashMap<u32, (Option<ObjectMetadata>, State)> =
        list_vni_rsp
            .virtual_networks
            .iter()
            .filter_map(|virtual_network| {
                let vni = virtual_network.spec.as_ref()?.vni;
                let metadata = virtual_network.metadata.clone();
                let state = if metadata.is_none() {
                    State {
                        phase: Phase::Error.into(),
                        reason: "Response is missing metadata".to_string(),
                        message: "list_virtual_networks".to_string(),
                    }
                } else {
                    virtual_network
                        .status
                        .as_ref()
                        .and_then(|status| status.state.clone())
                        .unwrap_or_else(|| State {
                            phase: Phase::Error.into(),
                            reason: "Response is missing state".to_string(),
                            message: "list_virtual_networks".to_string(),
                        })
                };
                Some((vni, (metadata, state)))
            })
            .collect();

    // For each attachment in astra status get the {vni, virtual-network-id}.
    let mut virtual_network_update_states: HashMap<u32, State> = HashMap::new();
    for astra_attachment_status in &mut astra_config_status.astra_attachments_status {
        // A vni of 0 indicates an admin-network. The Weave server rejects
        // vni 0, so skip sending it to weave.
        if astra_attachment_status.vni == 0 {
            continue;
        }

        // Compare astra {vni, virtual-network} against the
        // seen_virtual_networks_hashmap {vni, virtual-network-id}. Special
        // process matching virtual networks that exist on the weave server.
        let astra_vni = astra_attachment_status.vni as u32;
        let astra_virtual_network_id =
            astra_weave_ew_vpc_virtual_network_id(astra_attachment_status.vni);

        if let Some((virtual_network_metadata, weave_ew_vpc_state)) =
            seen_virtual_networks_hashmap.get(&astra_vni)
        {
            process_matching_vni_weave_ew_vpc_virtual_network(
                socket_path,
                astra_attachment_status,
                &astra_virtual_network_id,
                virtual_network_metadata,
                weave_ew_vpc_state,
                &mut virtual_network_update_states,
            )
            .await;
            continue;
        }

        // At this point we don't have a matching virtual network for the
        // on the Doca Weave server, create it and process response. Mark
        // the astra attachment status with an error if the API fails.
        // Note that we have to insert any newly created virtual networks
        // on the server in the seen_virtual_networks HashMap to avoid
        // duplicate creation of virtual networks on the server where there
        // is more than one attachment matching the same (new) vni.
        let astra_subnet_ipv4 = format!(
            "{}/{}",
            astra_attachment_status.subnet_ipv4, astra_attachment_status.subnet_mask
        );

        let create_vni_req = CreateVirtualNetworkRequest {
            metadata: Some(weave_ew_vpc_object_metadata(
                Some(astra_weave_ew_vpc_virtual_network_id(
                    astra_attachment_status.vni,
                )),
                &astra_attachment_status.revision,
            )),
            spec: Some(VirtualNetworkSpec {
                vni: astra_attachment_status.vni as u32,
                subnet_ipv4: Some(astra_subnet_ipv4.clone()),
                subnet_ipv6: None,
            }),
        };

        let create_vni_rsp = weave_ew_vpc_create_virtual_network(socket_path, create_vni_req).await;
        let weave_ew_vpc_state = match create_vni_rsp {
            Ok(create_vni_rsp) => match create_vni_rsp.virtual_network {
                Some(virtual_network) => {
                    let weave_ew_vpc_state = virtual_network
                        .status
                        .and_then(|status| status.state)
                        .unwrap_or_else(|| State {
                            phase: Phase::Error.into(),
                            reason: "Response is missing state".to_string(),
                            message: "create_virtual_network".to_string(),
                        });
                    seen_virtual_networks_hashmap.insert(
                        astra_vni,
                        (virtual_network.metadata, weave_ew_vpc_state.clone()),
                    );
                    // Creation already wrote this revision, so later attachments sharing this
                    // VNI must reuse its result rather than send a redundant update.
                    virtual_network_update_states.insert(astra_vni, weave_ew_vpc_state.clone());
                    weave_ew_vpc_state
                }
                None => State {
                    phase: Phase::Error.into(),
                    reason: "Response is missing virtual network".to_string(),
                    message: "create_virtual_network".to_string(),
                },
            },
            Err(err) => State {
                phase: Phase::Error.into(),
                reason: "API failure".to_string(),
                message: format!("create_virtual_network: {err:#}"),
            },
        };

        if weave_ew_vpc_state.phase != Phase::Ready as i32 {
            tracing::info!(
                ?astra_attachment_status,
                phase = weave_ew_vpc_state.phase,
                reason = %weave_ew_vpc_state.reason,
                message = %weave_ew_vpc_state.message,
                "Weave EW VPC virtual network created in non-ready phase"
            );
            set_astra_attachment_status_with_weave_ew_vpc_status(
                astra_attachment_status,
                weave_ew_vpc_state,
            );
            continue;
        }

        tracing::info!(?astra_attachment_status, "Created virtual network");
    }

    Ok(())
}

// Process virtual networks whose VNI matches an Astra attachment.
// For virtual networks that are in error state, or whose virtual
// network id mismatches copy the error into astra status. Then if
// we have valid metadata returned by the weave server, update our
// latest revision string on the weave server using the resource_version
// that the server expects in the update. Note that since a virtual network
// can have multiple attachments, cache the first update result for each
// VNI and reuse it for later attachments.
async fn process_matching_vni_weave_ew_vpc_virtual_network(
    socket_path: &str,
    astra_attachment_status: &mut AstraAttachmentStatus,
    astra_virtual_network_id: &str,
    virtual_network_metadata: &Option<ObjectMetadata>,
    weave_ew_vpc_state: &State,
    virtual_network_update_states: &mut HashMap<u32, State>,
) {
    let weave_ew_vpc_is_ready = weave_ew_vpc_state.phase == Phase::Ready as i32;
    let virtual_network_id_matches = virtual_network_metadata
        .as_ref()
        .and_then(|metadata| metadata.id.as_deref())
        == Some(astra_virtual_network_id);
    tracing::info!(
        vni = astra_attachment_status.vni,
        weave_phase = weave_ew_vpc_state.phase,
        virtual_network_id_matches,
        "Processing matching Weave virtual network"
    );

    if !weave_ew_vpc_is_ready {
        set_astra_attachment_status_with_weave_ew_vpc_status(
            astra_attachment_status,
            weave_ew_vpc_state.clone(),
        );
    } else if !virtual_network_id_matches {
        set_astra_attachment_status_with_weave_ew_vpc_status(
            astra_attachment_status,
            State {
                phase: Phase::Error.into(),
                reason: "Conflicting DOCA Weave virtual network ID".to_string(),
                message: format!(
                    "VNI {} exists as {:?}, expected {astra_virtual_network_id}",
                    astra_attachment_status.vni,
                    virtual_network_metadata
                        .as_ref()
                        .and_then(|metadata| metadata.id.as_deref())
                ),
            },
        );
    }

    let Some(virtual_network_metadata) = virtual_network_metadata.as_ref() else {
        tracing::info!(
            vni = astra_attachment_status.vni,
            "Cannot update matching Weave virtual network because metadata is missing"
        );
        return;
    };

    let astra_vni = astra_attachment_status.vni as u32;
    let update_state = if let Some(update_state) = virtual_network_update_states.get(&astra_vni) {
        update_state.clone()
    } else {
        // Preserve the metadata returned by Weave, including its resource
        // version, and only replace NICo's revision value.
        let mut metadata = virtual_network_metadata.clone();
        metadata.user_data.insert(
            WEAVE_EW_VPC_REVISION_USER_DATA_KEY.to_string(),
            astra_attachment_status.revision.clone(),
        );
        let request = UpdateVirtualNetworkRequest {
            metadata: Some(metadata),
        };
        let update_state = match weave_ew_vpc_update_virtual_network(socket_path, request).await {
            Ok(response) => response
                .virtual_network
                .and_then(|virtual_network| virtual_network.status)
                .and_then(|status| status.state)
                .unwrap_or_else(|| State {
                    phase: Phase::Error.into(),
                    reason: "Response is missing state".to_string(),
                    message: "update_virtual_network".to_string(),
                }),
            Err(err) => {
                tracing::info!(
                    vni = astra_vni,
                    error = format!("{err:#}"),
                    "Weave virtual network revision update failed"
                );
                State {
                    phase: Phase::Error.into(),
                    reason: "API failure".to_string(),
                    message: format!("update_virtual_network: {err:#}"),
                }
            }
        };
        virtual_network_update_states.insert(astra_vni, update_state.clone());
        update_state
    };
    if weave_ew_vpc_is_ready
        && virtual_network_id_matches
        && update_state.phase != Phase::Ready as i32
    {
        set_astra_attachment_status_with_weave_ew_vpc_status(astra_attachment_status, update_state);
    }
}

// This routine queries the DOCA Weave server for the list of virtual networks
// and deletes stale virtual networks that are no longer present in the Astra
// config.
async fn delete_stale_weave_ew_vpc_virtual_networks(
    socket_path: &str,
    astra_config_status: &AstraConfigStatus,
) -> eyre::Result<()> {
    let list_vni_req = ListVirtualNetworksRequest { vni: None };
    let list_vni_rsp = weave_ew_vpc_list_virtual_networks(socket_path, list_vni_req).await?;

    // Walk virtual networks on the Doca Weave server.
    for virtual_network in list_vni_rsp.virtual_networks {
        // Skip deletion of virtual network that is in the astra config
        if weave_ew_vpc_virtual_network_matches_astra_config(&virtual_network, astra_config_status)
        {
            continue;
        }

        // Log error if deletion encounters entry with invalid metadata.
        let Some(delete_vni_id) = virtual_network
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.id.clone())
            .none_if_empty()
        else {
            tracing::error!(
                ?virtual_network,
                "ERROR: Cannot delete virtual network from DOCA Weave server because metadata id is missing or empty"
            );
            continue;
        };

        // Delete stale virtual network.
        let delete_vni_req = DeleteVirtualNetworkRequest {
            id: delete_vni_id.clone(),
        };
        match weave_ew_vpc_delete_virtual_network(socket_path, delete_vni_req).await {
            Ok(_) => {
                tracing::info!(
                    ?virtual_network,
                    "Requested deletion of stale virtual network from DOCA Weave server"
                );
            }
            Err(err) => {
                return Err(eyre::eyre!(
                    "failed to delete stale virtual network from DOCA weave server: {err:#}"
                ));
            }
        }
    }

    Ok(())
}

fn weave_ew_vpc_virtual_network_matches_astra_config(
    virtual_network: &crate::weave_ew_vpc_client::proto::VirtualNetwork,
    astra_config_status: &AstraConfigStatus,
) -> bool {
    let Some(vni) = virtual_network.spec.as_ref().map(|spec| spec.vni) else {
        return false;
    };

    // vni 0 is the admin network and never corresponds to a real virtual
    // network, so a vni 0 network on the server is always stale.
    if vni == 0 {
        return false;
    }

    astra_config_status
        .astra_attachments_status
        .iter()
        .any(|astra_attachment_status| astra_attachment_status.vni as u32 == vni)
}

// Take a diff of Doca Weave Server vs Astra attachments and create new
// or delete non-existing attachments.
// For existing attachments, handle special case where an attachment may
// have changed its partition aka vni. This case is handled by deleting
// the existing attachment and recreating a new one with the new VNI.
// For existing attachments that we don't recreate, we need to update
// the revision string on the weave server.
async fn create_update_weave_ew_vpc_astra_attachments(
    socket_path: &str,
    astra_config_status: &mut AstraConfigStatus,
) -> eyre::Result<()> {
    // Get list of vni attachments from DOCA Weave server.
    let list_vni_attachments_req = ListVirtualNetworkAttachmentsRequest {
        vnet_id: None,
        nic_id: None,
    };
    let list_vni_attachments_rsp =
        weave_ew_vpc_list_virtual_network_attachments(socket_path, list_vni_attachments_req)
            .await?;

    debug_check_weave_ew_vpc_attachment_revisions(
        &list_vni_attachments_rsp.virtual_network_attachments,
    );

    log_virtual_network_attachments(&list_vni_attachments_rsp.virtual_network_attachments);

    // Track attachments we delete during reconcile so the stale-attachment
    // pass below does not attempt to delete them a second time.
    let mut deleted_attachment_ids = HashSet::new();

    // Diff Doca Weave Server vs AstraAttachments to create new attachments
    // and delete/recreate attachments where the partition (vni) has changed.
    for astra_attachment_status in &mut astra_config_status.astra_attachments_status {
        // Skip any attachments where the status is not ready. The non-
        // ready status was filled in during virtual network processing.
        if astra_attachment_status
            .status
            .as_ref()
            .is_none_or(|status| status.phase != AstraPhase::PhaseReady as i32)
        {
            continue;
        }

        // A vni of 0 is for admin network, don't create an attachment for it.
        if astra_attachment_status.vni == 0 {
            continue;
        }

        // Reject an unbuildable replacement before deleting an attachment with
        // the same NIC and VNI. Stale cleanup below follows the desired NIC/VNI set.
        let desired_spec = match weave_ew_virtual_network_attachment_spec_from_astra_attachment(
            astra_attachment_status,
        ) {
            Ok(spec) => spec,
            Err(state) => {
                set_astra_attachment_status_with_weave_ew_vpc_status(
                    astra_attachment_status,
                    state,
                );
                continue;
            }
        };

        let astra_virtual_network_id =
            astra_weave_ew_vpc_virtual_network_id(astra_attachment_status.vni);

        // Build same_nic_attachments vector of all attachments on the DOCA
        // Weave server that have the same NIC MAC address as the Astra
        // Attachment (should be only one).
        let same_nic_attachments = list_vni_attachments_rsp
            .virtual_network_attachments
            .iter()
            .filter(|virtual_network_attachment| {
                virtual_network_attachment
                    .spec
                    .as_ref()
                    .is_some_and(|spec| {
                        spec.nic_id
                            .eq_ignore_ascii_case(astra_attachment_status.mac_address.as_str())
                    })
            })
            .collect::<Vec<_>>();

        // Build list of conflicting attachments for matching attachments where
        // the vni changed (should be one or none).
        let mut exact_attachment = None;
        let mut conflicting_attachments = Vec::new();
        for virtual_network_attachment in same_nic_attachments {
            if virtual_network_attachment
                .spec
                .as_ref()
                .is_some_and(|spec| spec.vnet_id == astra_virtual_network_id)
            {
                exact_attachment = Some(virtual_network_attachment);
            } else {
                conflicting_attachments.push(virtual_network_attachment);
            }
        }

        // Delete matching attachments with conflicting vni. Track
        // deleted attachments in the deleted_attachment_ids HashSet.
        let mut all_conflicting_attachments_deleted = true;
        for conflicting_attachment in conflicting_attachments {
            let deleted = delete_match_attachment_with_vni_changed(
                socket_path,
                Some(conflicting_attachment),
                &mut deleted_attachment_ids,
                astra_attachment_status,
            )
            .await?;
            if !deleted {
                all_conflicting_attachments_deleted = false;
            }
        }
        if !all_conflicting_attachments_deleted {
            continue;
        }

        // Matching NIC and VNI values allow a revision update only when the
        // full binding is unchanged. Otherwise request deletion of the old
        // attachment and create a replacement with the desired spec.
        if let Some(exact_attachment) = exact_attachment {
            if !weave_ew_vpc_attachment_matches_desired_spec(
                exact_attachment,
                astra_attachment_status,
            ) {
                let deleted = delete_match_attachment_with_vni_changed(
                    socket_path,
                    Some(exact_attachment),
                    &mut deleted_attachment_ids,
                    astra_attachment_status,
                )
                .await?;
                if !deleted {
                    continue;
                }
            } else {
                process_matching_weave_ew_vpc_virtual_network_attachment(
                    socket_path,
                    astra_attachment_status,
                    exact_attachment,
                )
                .await;
                continue;
            }
        }

        // create new or recreate mismatched vni attachments.
        create_or_recreate_weave_ew_vpc_astra_attachment(
            socket_path,
            astra_attachment_status,
            desired_spec,
        )
        .await?;
    }

    // Delete attachments on the server that are not in the astra config list.
    delete_stale_weave_ew_vpc_astra_attachments(
        socket_path,
        &list_vni_attachments_rsp.virtual_network_attachments,
        &mut deleted_attachment_ids,
        astra_config_status,
    )
    .await?;

    Ok(())
}

// Process an attachment that already exists on Weave: report its current
// status, then update NICo's revision while preserving Weave's metadata and
// resource version. Metadata must be present to send the update.
async fn process_matching_weave_ew_vpc_virtual_network_attachment(
    socket_path: &str,
    astra_attachment_status: &mut AstraAttachmentStatus,
    virtual_network_attachment: &VirtualNetworkAttachment,
) {
    tracing::info!(
        vni = astra_attachment_status.vni,
        mac_address = %astra_attachment_status.mac_address,
        "Processing matching Weave virtual network attachment"
    );

    // Check for existence of metadata, update astra status with error
    // if metadata is not present.
    let Some(mut metadata) = virtual_network_attachment.metadata.clone() else {
        tracing::info!(
            vni = astra_attachment_status.vni,
            mac_address = %astra_attachment_status.mac_address,
            "Cannot update matching Weave virtual network attachment because metadata is missing"
        );
        set_astra_attachment_status_with_weave_ew_vpc_status(
            astra_attachment_status,
            State {
                phase: Phase::Error.into(),
                reason: "Response is missing metadata".to_string(),
                message: "list_virtual_network_attachments".to_string(),
            },
        );
        return;
    };

    // Update the Astra attachment status with the Weave error status.
    let weave_ew_vpc_state = virtual_network_attachment
        .status
        .as_ref()
        .and_then(|status| status.state.clone())
        .unwrap_or_else(|| State {
            phase: Phase::Error.into(),
            reason: "Missing Doca Weave Server Status State".to_string(),
            message: "list_virtual_network_attachments".to_string(),
        });
    let weave_ew_vpc_is_ready = weave_ew_vpc_state.phase == Phase::Ready as i32;
    set_astra_attachment_status_with_weave_ew_vpc_status(
        astra_attachment_status,
        weave_ew_vpc_state,
    );

    // Notify revision update to weave server.
    metadata.user_data.insert(
        WEAVE_EW_VPC_REVISION_USER_DATA_KEY.to_string(),
        astra_attachment_status.revision.clone(),
    );
    let request = UpdateVirtualNetworkAttachmentRequest {
        metadata: Some(metadata),
    };
    let update_state =
        match weave_ew_vpc_update_virtual_network_attachment(socket_path, request).await {
            Ok(response) => response
                .virtual_network_attachment
                .and_then(|virtual_network_attachment| virtual_network_attachment.status)
                .and_then(|status| status.state)
                .unwrap_or_else(|| State {
                    phase: Phase::Error.into(),
                    reason: "Response is missing state".to_string(),
                    message: "update_virtual_network_attachment".to_string(),
                }),
            Err(err) => {
                tracing::info!(
                    vni = astra_attachment_status.vni,
                    mac_address = %astra_attachment_status.mac_address,
                    error = format!("{err:#}"),
                    "Weave virtual network attachment revision update failed"
                );
                State {
                    phase: Phase::Error.into(),
                    reason: "API failure".to_string(),
                    message: format!("update_virtual_network_attachment: {err:#}"),
                }
            }
        };
    if weave_ew_vpc_is_ready && update_state.phase != Phase::Ready as i32 {
        set_astra_attachment_status_with_weave_ew_vpc_status(astra_attachment_status, update_state);
    }
}

async fn create_or_recreate_weave_ew_vpc_astra_attachment(
    socket_path: &str,
    astra_attachment_status: &mut AstraAttachmentStatus,
    desired_spec: VirtualNetworkAttachmentSpec,
) -> eyre::Result<()> {
    let weave_ew_vpc_attachment_create_req = CreateVirtualNetworkAttachmentRequest {
        metadata: Some(weave_ew_vpc_object_metadata(
            None,
            &astra_attachment_status.revision,
        )),
        spec: Some(desired_spec),
    };

    let weave_ew_vpc_attachment_create_rsp = weave_ew_vpc_create_virtual_network_attachment(
        socket_path,
        weave_ew_vpc_attachment_create_req,
    )
    .await;
    let weave_ew_vpc_state = match weave_ew_vpc_attachment_create_rsp {
        Ok(weave_ew_vpc_attachment_create_rsp) => weave_ew_vpc_attachment_create_rsp
            .virtual_network_attachment
            .and_then(|virtual_network_attachment| virtual_network_attachment.status)
            .and_then(|status| status.state)
            .unwrap_or_else(|| State {
                phase: Phase::Error.into(),
                reason: "Missing Doca Weave Server Status State".to_string(),
                message: "create_virtual_network_attachment".to_string(),
            }),
        Err(err) => State {
            phase: Phase::Error.into(),
            reason: "API Failed".to_string(),
            message: format!("create_virtual_network_attachment: {err:#}"),
        },
    };

    if weave_ew_vpc_state.phase != Phase::Ready as i32 {
        tracing::info!(
            ?astra_attachment_status,
            phase = weave_ew_vpc_state.phase,
            reason = %weave_ew_vpc_state.reason,
            message = %weave_ew_vpc_state.message,
            "Weave EW VPC virtual network attachment creation did not return Phase::Ready"
        );
        set_astra_attachment_status_with_weave_ew_vpc_status(
            astra_attachment_status,
            weave_ew_vpc_state,
        );
        return Ok(());
    }

    tracing::info!(
        ?astra_attachment_status,
        "Created virtual network attachment"
    );

    Ok(())
}

async fn delete_stale_weave_ew_vpc_astra_attachments(
    socket_path: &str,
    weave_ew_vpc_attachments: &[VirtualNetworkAttachment],
    deleted_attachment_ids: &mut HashSet<String>,
    astra_config_status: &AstraConfigStatus,
) -> eyre::Result<()> {
    for virtual_network_attachment in weave_ew_vpc_attachments {
        // Skip already deleted attachments in the deleted_attachment_id HashSet
        if virtual_network_attachment
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.id.as_ref())
            .is_some_and(|id| deleted_attachment_ids.contains(id))
        {
            continue;
        }

        // Skip if the server attachment matches the astra config.
        if weave_ew_vpc_attachment_exists_in_astra_config(
            virtual_network_attachment,
            astra_config_status,
        ) {
            continue;
        }

        // Delete mismatched attachment.
        let Some(del_attachment_id) = virtual_network_attachment
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.id.clone())
            .none_if_empty()
        else {
            tracing::error!(
                ?virtual_network_attachment,
                "ERROR: Cannot delete virtual network attachment from DOCA Weave server because metadata id is missing or empty"
            );
            continue;
        };

        let weave_ew_vpc_del_attachment_req = DeleteVirtualNetworkAttachmentRequest {
            id: del_attachment_id.clone(),
        };

        match weave_ew_vpc_delete_virtual_network_attachment(
            socket_path,
            weave_ew_vpc_del_attachment_req,
        )
        .await
        {
            Ok(_) => {
                deleted_attachment_ids.insert(del_attachment_id);
                tracing::info!(
                    ?virtual_network_attachment,
                    "Requested deletion of stale virtual network attachment from DOCA Weave server"
                );
            }
            Err(err) => {
                return Err(eyre::eyre!(
                    "failed to delete stale virtual network attachment from DOCA weave server: {err:#}"
                ));
            }
        }
    }

    Ok(())
}

fn weave_ew_vpc_attachment_exists_in_astra_config(
    virtual_network_attachment: &VirtualNetworkAttachment,
    astra_config_status: &AstraConfigStatus,
) -> bool {
    astra_config_status
        .astra_attachments_status
        .iter()
        .any(|astra_attachment_status| {
            virtual_network_attachment
                .spec
                .as_ref()
                .is_some_and(|spec| {
                    spec.nic_id
                        .eq_ignore_ascii_case(astra_attachment_status.mac_address.as_str())
                        && spec.vnet_id
                            == astra_weave_ew_vpc_virtual_network_id(astra_attachment_status.vni)
                })
        })
}

// Weave can update revision metadata in place, but a changed binding needs
// a replacement attachment. Compare the full spec so PF/VF drift follows the
// same repair path as an OVS bridge or attachment type change.
fn weave_ew_vpc_attachment_matches_desired_spec(
    existing_attachment: &VirtualNetworkAttachment,
    astra_attachment_status: &AstraAttachmentStatus,
) -> bool {
    let Some(mut existing_spec) = existing_attachment.spec.clone() else {
        return false;
    };
    let Ok(mut desired_spec) =
        weave_ew_virtual_network_attachment_spec_from_astra_attachment(astra_attachment_status)
    else {
        return false;
    };

    for spec in [&mut existing_spec, &mut desired_spec] {
        spec.nic_id.make_ascii_lowercase();
        if let Some(pf) = spec.attachment_pf.as_mut() {
            pf.pf_id.make_ascii_lowercase();
        }
        if let Some(vf) = spec.attachment_vf.as_mut() {
            vf.pf_id.make_ascii_lowercase();
        }
    }
    existing_spec == desired_spec
}

// A successful Delete accepts the request; the final observation still has to
// prove disappearance. On failure, record the attachment error and return false
// so the caller does not create its replacement in this iteration.
async fn delete_match_attachment_with_vni_changed(
    socket_path: &str,
    match_attachment: Option<&VirtualNetworkAttachment>,
    deleted_attachment_ids: &mut HashSet<String>,
    astra_attachment_status: &mut AstraAttachmentStatus,
) -> eyre::Result<bool> {
    // Get delete attachment metadata
    let Some(delete_attachment_id) = match_attachment
        .and_then(|attachment| attachment.metadata.as_ref())
        .and_then(|metadata| metadata.id.clone())
        .none_if_empty()
    else {
        tracing::error!(
            ?match_attachment,
            "ERROR: Cannot delete mismatched virtual network attachment from DOCA Weave server as metadata id missing or empty"
        );
        set_astra_attachment_status_with_weave_ew_vpc_status(
            &mut *astra_attachment_status,
            State {
                phase: Phase::Error.into(),
                reason: "Missing Doca Weave attachment ID".to_string(),
                message: "delete_virtual_network_attachment".to_string(),
            },
        );
        return Ok(false);
    };

    // Delete virtual network attachment and handle error
    let weave_ew_vpc_attachment_del_req = DeleteVirtualNetworkAttachmentRequest {
        id: delete_attachment_id.clone(),
    };
    match weave_ew_vpc_delete_virtual_network_attachment(
        socket_path,
        weave_ew_vpc_attachment_del_req,
    )
    .await
    {
        Ok(_) => {
            deleted_attachment_ids.insert(delete_attachment_id);
            tracing::info!(
                ?match_attachment,
                "Requested deletion of mismatched virtual network attachment from DOCA Weave server"
            );
        }
        Err(err) => {
            tracing::error!(
                error = format!("{err:#}"),
                ?match_attachment,
                "ERROR: Failed to delete mismatched virtual network attachment from DOCA Weave server"
            );
            set_astra_attachment_status_with_weave_ew_vpc_status(
                &mut *astra_attachment_status,
                State {
                    phase: Phase::Error.into(),
                    reason: "Failed to delete mismatched Doca Weave attachment".to_string(),
                    message: format!("delete_virtual_network_attachment: {err:#}"),
                },
            );
            return Ok(false);
        }
    };
    Ok(true)
}

#[cfg(test)]
tokio::task_local! {
    /// Directs one test agent task to its own Weave service without changing other tasks.
    pub(crate) static TEST_WEAVE_EW_VPC_SOCKET_PATH: String;
}

/// Reconciles Astra configuration during an agent main-loop iteration.
/// `None` returns an empty status without contacting Weave. `Ok` contains Ready
/// only when the complete service state matches; otherwise entries carry
/// Pending, Error, or another non-ready service phase. An invalid desired VNI or
/// revision returns `Err`, as do observation failures and stale cleanup failures.
/// Incomplete cleanup with an empty desired configuration also returns `Err`,
/// because no attachment can hold its diagnostic.
pub(super) async fn build_notify_weave_ew_vpc_astra_config(
    astra_config: Option<&AstraConfig>,
) -> eyre::Result<AstraConfigStatus> {
    #[cfg(test)]
    if let Ok(socket_path) = TEST_WEAVE_EW_VPC_SOCKET_PATH.try_with(Clone::clone) {
        return build_notify_weave_ew_vpc_astra_config_uds(&socket_path, astra_config).await;
    }
    build_notify_weave_ew_vpc_astra_config_uds(
        WEAVE_EW_VPC_FLOW_CONTROLLER_SOCKET_PATH,
        astra_config,
    )
    .await
}

async fn build_notify_weave_ew_vpc_astra_config_uds(
    socket_path: &str,
    astra_config: Option<&AstraConfig>,
) -> eyre::Result<AstraConfigStatus> {
    let Some(astra_config) = astra_config else {
        return Ok(AstraConfigStatus {
            astra_attachments_status: Vec::new(),
            is_attachment_status_present: Some(false),
            is_maintenance_status_present: Some(false),
            ..Default::default()
        });
    };
    debug_check_astra_config_attachment_revisions(astra_config);
    let desired = build_astra_config_status(astra_config)?;

    // A matching revision alone does not prove convergence: stale networks,
    // changed bindings, and pending operations can survive between iterations.
    let (networks, attachments) = read_weave_ew_vpc_resources(socket_path).await?;
    match verify_weave_ew_vpc_resources_match(&desired, &networks, &attachments) {
        Ok(()) => return Ok(desired),
        Err(err) => tracing::debug!(
            error = %err,
            "DOCA Weave state has not converged; running full reconcile"
        ),
    }

    log_astra_config(astra_config);

    // Ready is an internal processing sentinel until the final observation.
    // Reconciliation replaces it with diagnostics when an operation fails.
    let mut astra_config_status = desired.clone();
    create_update_ew_vpc_virtual_networks(socket_path, &mut astra_config_status).await?;
    create_update_weave_ew_vpc_astra_attachments(socket_path, &mut astra_config_status).await?;
    delete_stale_weave_ew_vpc_virtual_networks(socket_path, &astra_config_status).await?;

    // Delete acknowledges the request before the resource necessarily
    // disappears. The complete service state gates every NIC, including a
    // detached NIC whose stale resources have no desired attachment to match.
    // Later main-loop iterations retry resources that still need work.
    let (networks, attachments) = read_weave_ew_vpc_resources(socket_path).await?;
    match verify_weave_ew_vpc_resources_match(&desired, &networks, &attachments) {
        // A failed RPC may still have applied its mutation. Fresh, complete
        // observations let us report Ready instead of retaining that RPC error.
        Ok(()) => Ok(desired),
        Err(err) => {
            if astra_config_status.astra_attachments_status.is_empty() {
                // No attachment can carry the Pending reason. Return it as an
                // error so the main loop can report why cleanup is incomplete.
                return Err(err);
            }
            for attachment in &mut astra_config_status.astra_attachments_status {
                if attachment
                    .status
                    .as_ref()
                    .is_some_and(|status| status.phase == AstraPhase::PhaseReady as i32)
                {
                    attachment.status = Some(AstraStatus {
                        phase: AstraPhase::PhasePending.into(),
                        reason: "DOCA Weave state has not converged".to_string(),
                        message: err.to_string(),
                    });
                }
            }
            Ok(astra_config_status)
        }
    }
}

async fn read_weave_ew_vpc_resources(
    socket_path: &str,
) -> eyre::Result<(Vec<VirtualNetwork>, Vec<VirtualNetworkAttachment>)> {
    let networks =
        weave_ew_vpc_list_virtual_networks(socket_path, ListVirtualNetworksRequest { vni: None })
            .await?;
    let attachments = weave_ew_vpc_list_virtual_network_attachments(
        socket_path,
        ListVirtualNetworkAttachmentsRequest {
            vnet_id: None,
            nic_id: None,
        },
    )
    .await?;
    Ok((
        networks.virtual_networks,
        attachments.virtual_network_attachments,
    ))
}

fn log_astra_config(astra_config: &AstraConfig) {
    tracing::info!(
        attachment_count = astra_config.astra_attachments.len(),
        "Input Astra config"
    );
    for astra_attachment in &astra_config.astra_attachments {
        tracing::info!(?astra_attachment, "Input Astra config entry");
    }
}

fn log_virtual_networks(virtual_networks: &[crate::weave_ew_vpc_client::proto::VirtualNetwork]) {
    tracing::info!(
        virtual_network_count = virtual_networks.len(),
        "List VNI response"
    );
    for virtual_network in virtual_networks {
        tracing::info!(?virtual_network, "List VNI response entry");
    }
}

fn log_virtual_network_attachments(virtual_network_attachments: &[VirtualNetworkAttachment]) {
    tracing::info!(
        virtual_network_attachment_count = virtual_network_attachments.len(),
        "List VNI attachment response"
    );
    for virtual_network_attachment in virtual_network_attachments {
        tracing::info!(
            ?virtual_network_attachment,
            "List VNI attachment response entry"
        );
    }
}

fn build_astra_config_status(astra_config: &AstraConfig) -> eyre::Result<AstraConfigStatus> {
    let mut astra_config_status = AstraConfigStatus {
        astra_attachments_status: Vec::new(),
        is_attachment_status_present: Some(true),
        is_maintenance_status_present: Some(false),
        ..Default::default()
    };

    for astra_attachment in &astra_config.astra_attachments {
        let revision = astra_attachment_revision(astra_attachment)?;
        let astra_attachment_status = AstraAttachmentStatus {
            mac_address: astra_attachment.mac_address.clone(),
            vni: i32::try_from(astra_attachment.vni)
                .wrap_err_with(|| format!("VNI {} does not fit in i32", astra_attachment.vni))?,
            subnet_ipv4: astra_attachment.subnet_ipv4.clone(),
            subnet_mask: astra_attachment.subnet_mask,
            attachment_type: astra_attachment.attachment_type,
            attachment_vf: astra_attachment.attachment_vf,
            attachment_ovs: astra_attachment.attachment_ovs.clone(),
            revision: revision.to_string(),
            status: Some(AstraStatus {
                phase: AstraPhase::PhaseReady.into(),
                reason: String::new(),
                message: String::new(),
            }),
        };
        astra_config_status
            .astra_attachments_status
            .push(astra_attachment_status);
    }

    Ok(astra_config_status)
}

fn astra_attachment_revision(
    astra_attachment: &::rpc::forge::AstraAttachment,
) -> eyre::Result<&str> {
    if astra_attachment.revision.is_empty() {
        return Err(eyre::eyre!(
            "missing revision for astra attachment {}",
            astra_attachment.mac_address
        ));
    }

    Ok(astra_attachment.revision.as_str())
}

fn weave_ew_vpc_attachment_revision(
    virtual_network_attachment: &VirtualNetworkAttachment,
) -> Option<&str> {
    virtual_network_attachment
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY))
        .map(String::as_str)
}

fn weave_ew_vpc_attachment_id(virtual_network_attachment: &VirtualNetworkAttachment) -> &str {
    virtual_network_attachment
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.id.as_deref())
        .filter(|id| !id.is_empty())
        .unwrap_or("unknown")
}

// Debug-only consistency checks. These log errors but do not fail reconcile.
// Checks that all attachments have the same revision.
fn debug_check_astra_config_attachment_revisions(astra_config: &AstraConfig) {
    let attachments = &astra_config.astra_attachments;
    if attachments.len() <= 1 {
        return;
    }

    let Some(first_attachment) = attachments.first() else {
        return;
    };

    if first_attachment.revision.is_empty() {
        tracing::error!(
            mac_address = %first_attachment.mac_address,
            "ERROR: AstraConfig attachment has missing revision"
        );
    }

    let first_revision = first_attachment.revision.as_str();
    for attachment in attachments.iter().skip(1) {
        if attachment.revision.is_empty() {
            tracing::error!(
                mac_address = %attachment.mac_address,
                "ERROR: AstraConfig attachment has missing revision"
            );
            continue;
        }

        if attachment.revision != first_revision {
            tracing::error!(
                first_mac_address = %first_attachment.mac_address,
                first_revision,
                mac_address = %attachment.mac_address,
                revision = %attachment.revision,
                "ERROR: AstraConfig attachment revisions are inconsistent"
            );
        }
    }
}

fn debug_check_weave_ew_vpc_attachment_revisions(
    virtual_network_attachments: &[VirtualNetworkAttachment],
) {
    if virtual_network_attachments.len() <= 1 {
        return;
    }

    let first_attachment = &virtual_network_attachments[0];
    let first_revision = weave_ew_vpc_attachment_revision(first_attachment);
    let first_attachment_id = weave_ew_vpc_attachment_id(first_attachment);

    for attachment in virtual_network_attachments.iter().skip(1) {
        let revision = weave_ew_vpc_attachment_revision(attachment);
        if revision != first_revision {
            tracing::error!(
                first_attachment_id,
                first_revision = first_revision.unwrap_or("none"),
                attachment_id = weave_ew_vpc_attachment_id(attachment),
                revision = revision.unwrap_or("none"),
                "ERROR: DOCA Weave virtual network attachment revisions are inconsistent"
            );
        }
    }
}

fn set_astra_attachment_status_with_weave_ew_vpc_status(
    astra_attachment_status: &mut AstraAttachmentStatus,
    weave_ew_vpc_state: State,
) {
    let astra_status_phase =
        match Phase::try_from(weave_ew_vpc_state.phase).unwrap_or(Phase::Unspecified) {
            Phase::Ready => AstraPhase::PhaseReady,
            Phase::Error => AstraPhase::PhaseError,
            Phase::Pending => AstraPhase::PhasePending,
            Phase::Deleting => AstraPhase::PhaseDeleting,
            _ => AstraPhase::PhaseUnspecified,
        };

    astra_attachment_status.status = Some(AstraStatus {
        phase: astra_status_phase.into(),
        reason: weave_ew_vpc_state.reason,
        message: weave_ew_vpc_state.message,
    });
    if astra_status_phase == AstraPhase::PhaseError {
        tracing::info!(
            ?astra_attachment_status,
            "ERROR: Updated Astra attachment status from Weave with an error"
        );
    } else {
        tracing::info!(
            ?astra_attachment_status,
            "Updated Astra attachment status from Weave"
        );
    }
}

fn verify_weave_ew_vpc_resources_match(
    desired_status: &AstraConfigStatus,
    virtual_networks: &[VirtualNetwork],
    virtual_network_attachments: &[VirtualNetworkAttachment],
) -> eyre::Result<()> {
    let desired_attachments = &desired_status.astra_attachments_status;
    let desired_vnis: HashSet<_> = desired_attachments
        .iter()
        .filter(|attachment| attachment.vni != 0)
        .map(|attachment| attachment.vni)
        .collect();
    eyre::ensure!(
        desired_vnis.len() == virtual_networks.len(),
        "virtual network count mismatch: expected {}, observed {}",
        desired_vnis.len(),
        virtual_networks.len()
    );

    let mut observed_vnis = HashSet::new();
    for network in virtual_networks {
        let spec = network
            .spec
            .as_ref()
            .ok_or_else(|| eyre::eyre!("DOCA weave virtual network is missing spec"))?;
        let desired = desired_attachments
            .iter()
            .find(|attachment| attachment.vni != 0 && attachment.vni as u32 == spec.vni)
            .ok_or_else(|| eyre::eyre!("unexpected DOCA weave virtual network VNI {}", spec.vni))?;
        eyre::ensure!(
            observed_vnis.insert(spec.vni),
            "duplicate virtual network VNI {}",
            spec.vni
        );
        let expected_id = astra_weave_ew_vpc_virtual_network_id(desired.vni);
        let metadata = network
            .metadata
            .as_ref()
            .ok_or_else(|| eyre::eyre!("virtual network {expected_id} is missing metadata"))?;
        eyre::ensure!(
            metadata.id.as_deref() == Some(expected_id.as_str())
                && metadata.deletion_timestamp.is_none(),
            "virtual network {expected_id} has an unexpected ID or is deleting"
        );
        // Reconcile writes one network revision per VNI, using its first
        // desired attachment. Each attachment's own revision is checked below.
        eyre::ensure!(
            metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY) == Some(&desired.revision),
            "virtual network {expected_id} revision does not match"
        );
        for attachment in desired_attachments
            .iter()
            .filter(|attachment| attachment.vni == desired.vni)
        {
            eyre::ensure!(
                spec.subnet_ipv4.as_deref()
                    == Some(
                        format!("{}/{}", attachment.subnet_ipv4, attachment.subnet_mask).as_str()
                    )
                    && spec.subnet_ipv6.is_none(),
                "virtual network {expected_id} subnet does not match"
            );
        }
        eyre::ensure!(
            network
                .status
                .as_ref()
                .and_then(|status| status.state.as_ref())
                .is_some_and(|state| state.phase == Phase::Ready as i32),
            "virtual network {expected_id} is not ready"
        );
    }

    let expected_attachment_count = desired_attachments
        .iter()
        .filter(|attachment| attachment.vni != 0)
        .count();
    eyre::ensure!(
        expected_attachment_count == virtual_network_attachments.len(),
        "attachment count mismatch: expected {expected_attachment_count}, observed {}",
        virtual_network_attachments.len()
    );
    let mut matched_indices = HashSet::new();
    let mut attachment_ids = HashSet::new();
    for desired in desired_attachments
        .iter()
        .filter(|attachment| attachment.vni != 0)
    {
        let (index, attachment) = virtual_network_attachments
            .iter()
            .enumerate()
            .find(|(index, attachment)| {
                !matched_indices.contains(index)
                    && weave_ew_vpc_attachment_matches_desired_spec(attachment, desired)
            })
            .ok_or_else(|| {
                eyre::eyre!(
                    "missing matching DOCA weave attachment for MAC {} VNI {}",
                    desired.mac_address,
                    desired.vni
                )
            })?;
        matched_indices.insert(index);
        let metadata = attachment
            .metadata
            .as_ref()
            .ok_or_else(|| eyre::eyre!("DOCA weave attachment is missing metadata"))?;
        let id = metadata
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| eyre::eyre!("DOCA weave attachment is missing ID"))?;
        eyre::ensure!(
            attachment_ids.insert(id),
            "duplicate DOCA weave attachment ID {id}"
        );
        eyre::ensure!(
            metadata.deletion_timestamp.is_none(),
            "attachment {id} is deleting"
        );
        eyre::ensure!(
            metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY) == Some(&desired.revision),
            "attachment {id} revision does not match"
        );
        eyre::ensure!(
            attachment
                .status
                .as_ref()
                .and_then(|status| status.state.as_ref())
                .is_some_and(|state| state.phase == Phase::Ready as i32),
            "attachment {id} is not ready"
        );
    }

    // Detached NICs have no service resources. They can report Ready only
    // after the same complete check has excluded all stale resources above.
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use ::rpc::forge as rpc;
    use tokio::net::UnixListener;
    use tokio::sync::Mutex;
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::{Request, Response, Status};

    use super::*;
    use crate::weave_ew_vpc_client::proto::network_isolation_service_server::{
        NetworkIsolationService, NetworkIsolationServiceServer,
    };
    use crate::weave_ew_vpc_client::proto::state::Phase as WeaveEwVpcPhase;
    use crate::weave_ew_vpc_client::proto::{self, State};

    #[derive(Default)]
    struct RecordedWeaveEwVpcCalls {
        list_virtual_networks: usize,
        create_virtual_networks: Vec<proto::CreateVirtualNetworkRequest>,
        update_virtual_networks: Vec<proto::UpdateVirtualNetworkRequest>,
        delete_virtual_networks: Vec<proto::DeleteVirtualNetworkRequest>,
        list_virtual_network_attachments: usize,
        create_virtual_network_attachments: Vec<proto::CreateVirtualNetworkAttachmentRequest>,
        update_virtual_network_attachments: Vec<proto::UpdateVirtualNetworkAttachmentRequest>,
        delete_virtual_network_attachments: Vec<proto::DeleteVirtualNetworkAttachmentRequest>,
    }

    // The mock completes operations immediately by default. Tests can keep
    // deleted resources visible, reject Lists, or hold an RPC after notifying
    // the test that it reached the service. Attachment failures can happen
    // before Delete changes state or after Create has stored its resource.
    #[derive(Default)]
    pub(crate) struct RecordedWeaveEwVpcState {
        virtual_networks: Vec<proto::VirtualNetwork>,
        virtual_network_attachments: Vec<proto::VirtualNetworkAttachment>,
        retain_deleted_resources: bool,
        pub(crate) reject_network_lists: bool,
        fail_network_list_on_call: Option<usize>,
        fail_attachment_list_on_call: Option<usize>,
        stall_network_list: Option<Arc<tokio::sync::Notify>>,
        stall_network_list_on_call: Option<usize>,
        stall_network_create: Option<Arc<tokio::sync::Notify>>,
        stall_attachment_list: Option<Arc<tokio::sync::Notify>>,
        attachment_delete_failure: Option<MockRpcFailure>,
        attachment_create_response_failure: Option<MockRpcFailure>,
    }

    /// Selects an error reply or a reply that stays pending after the request
    /// reaches the mock. Each handler chooses whether it has committed.
    #[derive(Clone, Debug)]
    enum MockRpcFailure {
        Unavailable,
        Stall(Arc<tokio::sync::Notify>),
    }

    impl MockRpcFailure {
        /// Returns a service error or leaves the reply pending until the caller's
        /// deadline expires. The state mutex must be released before this call.
        async fn status(self) -> Status {
            match self {
                Self::Unavailable => Status::unavailable("injected attachment RPC failure"),
                Self::Stall(entered) => {
                    entered.notify_one();
                    std::future::pending().await
                }
            }
        }
    }

    struct RecordingNetworkIsolationService {
        state: Arc<Mutex<RecordedWeaveEwVpcState>>,
        calls: Arc<Mutex<RecordedWeaveEwVpcCalls>>,
        create_virtual_network_phase: WeaveEwVpcPhase,
    }

    #[tonic::async_trait]
    impl NetworkIsolationService for RecordingNetworkIsolationService {
        async fn create_virtual_network(
            &self,
            request: Request<proto::CreateVirtualNetworkRequest>,
        ) -> Result<Response<proto::CreateVirtualNetworkResponse>, Status> {
            let request = request.into_inner();
            self.calls
                .lock()
                .await
                .create_virtual_networks
                .push(request.clone());
            let entered = self.state.lock().await.stall_network_create.clone();
            if let Some(entered) = entered {
                entered.notify_one();
                return std::future::pending().await;
            }

            let mut metadata = request.metadata.unwrap_or_default();
            if metadata.id.is_none() {
                let vni = request.spec.as_ref().map_or(0, |spec| spec.vni);
                metadata.id = Some(astra_weave_ew_vpc_virtual_network_id(vni as i32));
            }

            let virtual_network = proto::VirtualNetwork {
                metadata: Some(metadata),
                spec: request.spec,
                status: Some(proto::VirtualNetworkStatus {
                    state: Some(State {
                        phase: self.create_virtual_network_phase.into(),
                        reason: String::new(),
                        message: String::new(),
                    }),
                }),
            };
            self.state
                .lock()
                .await
                .virtual_networks
                .push(virtual_network.clone());

            Ok(Response::new(proto::CreateVirtualNetworkResponse {
                virtual_network: Some(virtual_network),
            }))
        }

        async fn delete_virtual_network(
            &self,
            request: Request<proto::DeleteVirtualNetworkRequest>,
        ) -> Result<Response<proto::DeleteVirtualNetworkResponse>, Status> {
            let request = request.into_inner();
            self.calls
                .lock()
                .await
                .delete_virtual_networks
                .push(request.clone());
            let mut state = self.state.lock().await;
            if state.retain_deleted_resources {
                for network in &mut state.virtual_networks {
                    if network
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.id.as_deref())
                        == Some(request.id.as_str())
                    {
                        network.metadata.as_mut().unwrap().deletion_timestamp =
                            Some(Default::default());
                        network
                            .status
                            .as_mut()
                            .unwrap()
                            .state
                            .as_mut()
                            .unwrap()
                            .phase = WeaveEwVpcPhase::Deleting.into();
                    }
                }
                return Ok(Response::new(proto::DeleteVirtualNetworkResponse {}));
            }
            state.virtual_networks.retain(|virtual_network| {
                virtual_network
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.id.as_deref())
                    != Some(request.id.as_str())
            });

            Ok(Response::new(proto::DeleteVirtualNetworkResponse {}))
        }

        async fn get_virtual_network(
            &self,
            _request: Request<proto::GetVirtualNetworkRequest>,
        ) -> Result<Response<proto::GetVirtualNetworkResponse>, Status> {
            Err(Status::unimplemented("not used by astra config tests"))
        }

        async fn update_virtual_network(
            &self,
            request: Request<proto::UpdateVirtualNetworkRequest>,
        ) -> Result<Response<proto::UpdateVirtualNetworkResponse>, Status> {
            let request = request.into_inner();
            self.calls
                .lock()
                .await
                .update_virtual_networks
                .push(request.clone());

            let metadata = request
                .metadata
                .ok_or_else(|| Status::invalid_argument("metadata is required"))?;
            let id = metadata
                .id
                .as_deref()
                .ok_or_else(|| Status::invalid_argument("metadata.id is required"))?;
            let mut state = self.state.lock().await;
            let virtual_network = state
                .virtual_networks
                .iter_mut()
                .find(|virtual_network| {
                    virtual_network
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.id.as_deref())
                        == Some(id)
                })
                .ok_or_else(|| Status::not_found("virtual network not found"))?;
            virtual_network.metadata = Some(metadata);

            Ok(Response::new(proto::UpdateVirtualNetworkResponse {
                virtual_network: Some(virtual_network.clone()),
            }))
        }

        async fn list_virtual_networks(
            &self,
            _request: Request<proto::ListVirtualNetworksRequest>,
        ) -> Result<Response<proto::ListVirtualNetworksResponse>, Status> {
            let call_count = {
                let mut calls = self.calls.lock().await;
                calls.list_virtual_networks += 1;
                calls.list_virtual_networks
            };
            let state = self.state.lock().await;
            if state.reject_network_lists || state.fail_network_list_on_call == Some(call_count) {
                return Err(Status::unavailable("injected virtual network list failure"));
            }
            if let Some(entered) = state.stall_network_list.clone()
                && state
                    .stall_network_list_on_call
                    .is_none_or(|call| call == call_count)
            {
                drop(state);
                entered.notify_one();
                return std::future::pending().await;
            }
            Ok(Response::new(proto::ListVirtualNetworksResponse {
                virtual_networks: state.virtual_networks.clone(),
            }))
        }

        async fn create_virtual_network_attachment(
            &self,
            request: Request<proto::CreateVirtualNetworkAttachmentRequest>,
        ) -> Result<Response<proto::CreateVirtualNetworkAttachmentResponse>, Status> {
            let request = request.into_inner();
            self.calls
                .lock()
                .await
                .create_virtual_network_attachments
                .push(request.clone());

            let mut metadata = request.metadata.unwrap_or_default();
            if metadata.id.is_none() {
                let id = request.spec.as_ref().map_or_else(
                    || "attachment-missing-spec".to_string(),
                    |spec| format!("attachment-{}-{}", spec.nic_id, spec.vnet_id),
                );
                metadata.id = Some(id);
            }

            let virtual_network_attachment = proto::VirtualNetworkAttachment {
                metadata: Some(metadata),
                spec: request.spec,
                status: Some(proto::VirtualNetworkAttachmentStatus {
                    state: Some(State {
                        phase: WeaveEwVpcPhase::Ready.into(),
                        reason: String::new(),
                        message: String::new(),
                    }),
                    host_ipv4: None,
                    host_ipv6: None,
                }),
            };
            let response_failure = {
                let mut state = self.state.lock().await;
                state
                    .virtual_network_attachments
                    .push(virtual_network_attachment.clone());
                state.attachment_create_response_failure.clone()
            };
            // The resource is already visible to Lists even if the caller
            // never receives a successful Create response.
            if let Some(failure) = response_failure {
                return Err(failure.status().await);
            }

            Ok(Response::new(
                proto::CreateVirtualNetworkAttachmentResponse {
                    virtual_network_attachment: Some(virtual_network_attachment),
                },
            ))
        }

        async fn delete_virtual_network_attachment(
            &self,
            request: Request<proto::DeleteVirtualNetworkAttachmentRequest>,
        ) -> Result<Response<proto::DeleteVirtualNetworkAttachmentResponse>, Status> {
            let request = request.into_inner();
            self.calls
                .lock()
                .await
                .delete_virtual_network_attachments
                .push(request.clone());
            let failure = self.state.lock().await.attachment_delete_failure.clone();
            if let Some(failure) = failure {
                return Err(failure.status().await);
            }
            let mut state = self.state.lock().await;
            if state.retain_deleted_resources {
                for attachment in &mut state.virtual_network_attachments {
                    if attachment
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.id.as_deref())
                        == Some(request.id.as_str())
                    {
                        attachment.metadata.as_mut().unwrap().deletion_timestamp =
                            Some(Default::default());
                        attachment
                            .status
                            .as_mut()
                            .unwrap()
                            .state
                            .as_mut()
                            .unwrap()
                            .phase = WeaveEwVpcPhase::Deleting.into();
                    }
                }
                return Ok(Response::new(
                    proto::DeleteVirtualNetworkAttachmentResponse {},
                ));
            }
            state
                .virtual_network_attachments
                .retain(|virtual_network_attachment| {
                    virtual_network_attachment
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.id.as_deref())
                        != Some(request.id.as_str())
                });

            Ok(Response::new(
                proto::DeleteVirtualNetworkAttachmentResponse {},
            ))
        }

        async fn get_virtual_network_attachment(
            &self,
            _request: Request<proto::GetVirtualNetworkAttachmentRequest>,
        ) -> Result<Response<proto::GetVirtualNetworkAttachmentResponse>, Status> {
            Err(Status::unimplemented("not used by astra config tests"))
        }

        async fn update_virtual_network_attachment(
            &self,
            request: Request<proto::UpdateVirtualNetworkAttachmentRequest>,
        ) -> Result<Response<proto::UpdateVirtualNetworkAttachmentResponse>, Status> {
            let request = request.into_inner();
            self.calls
                .lock()
                .await
                .update_virtual_network_attachments
                .push(request.clone());

            let metadata = request
                .metadata
                .ok_or_else(|| Status::invalid_argument("metadata is required"))?;
            let id = metadata
                .id
                .as_deref()
                .ok_or_else(|| Status::invalid_argument("metadata.id is required"))?;
            let mut state = self.state.lock().await;
            let virtual_network_attachment = state
                .virtual_network_attachments
                .iter_mut()
                .find(|virtual_network_attachment| {
                    virtual_network_attachment
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.id.as_deref())
                        == Some(id)
                })
                .ok_or_else(|| Status::not_found("virtual network attachment not found"))?;
            virtual_network_attachment.metadata = Some(metadata);

            Ok(Response::new(
                proto::UpdateVirtualNetworkAttachmentResponse {
                    virtual_network_attachment: Some(virtual_network_attachment.clone()),
                },
            ))
        }

        async fn list_virtual_network_attachments(
            &self,
            _request: Request<proto::ListVirtualNetworkAttachmentsRequest>,
        ) -> Result<Response<proto::ListVirtualNetworkAttachmentsResponse>, Status> {
            let call_count = {
                let mut calls = self.calls.lock().await;
                calls.list_virtual_network_attachments += 1;
                calls.list_virtual_network_attachments
            };
            let state = self.state.lock().await;
            if state.fail_attachment_list_on_call == Some(call_count) {
                return Err(Status::unavailable("injected attachment list failure"));
            }
            if let Some(entered) = state.stall_attachment_list.clone() {
                drop(state);
                entered.notify_one();
                return std::future::pending().await;
            }
            Ok(Response::new(
                proto::ListVirtualNetworkAttachmentsResponse {
                    virtual_network_attachments: state.virtual_network_attachments.clone(),
                },
            ))
        }
    }

    pub(crate) struct RecordingWeaveServer {
        _directory: tempfile::TempDir,
        task: tokio::task::JoinHandle<()>,
    }

    impl RecordingWeaveServer {
        /// Joins service cancellation before its temporary socket directory is
        /// removed, so one test cannot leave service work behind for another.
        pub(crate) async fn shutdown(mut self) -> eyre::Result<()> {
            self.task.abort();
            match (&mut self.task).await {
                Ok(()) => Ok(()),
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(error).wrap_err("recording weave service task failed"),
            }
        }
    }

    impl Drop for RecordingWeaveServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn start_recording_weave_ew_vpc_mock_server(
        virtual_networks: Vec<proto::VirtualNetwork>,
        virtual_network_attachments: Vec<proto::VirtualNetworkAttachment>,
    ) -> (
        PathBuf,
        Arc<Mutex<RecordedWeaveEwVpcCalls>>,
        RecordingWeaveServer,
    ) {
        start_recording_weave_ew_vpc_mock_server_with_create_phase(
            virtual_networks,
            virtual_network_attachments,
            WeaveEwVpcPhase::Ready,
        )
    }

    fn start_recording_weave_ew_vpc_mock_server_with_create_phase(
        virtual_networks: Vec<proto::VirtualNetwork>,
        virtual_network_attachments: Vec<proto::VirtualNetworkAttachment>,
        create_virtual_network_phase: WeaveEwVpcPhase,
    ) -> (
        PathBuf,
        Arc<Mutex<RecordedWeaveEwVpcCalls>>,
        RecordingWeaveServer,
    ) {
        let (path, calls, _, server) = start_recording_weave_ew_vpc_mock_server_with_state(
            virtual_networks,
            virtual_network_attachments,
            create_virtual_network_phase,
        );
        (path, calls, server)
    }

    /// Binds a private socket before starting the service and exposes its state
    /// and calls for reconciliation tests. The caller must join `shutdown`.
    fn start_recording_weave_ew_vpc_mock_server_with_state(
        virtual_networks: Vec<proto::VirtualNetwork>,
        virtual_network_attachments: Vec<proto::VirtualNetworkAttachment>,
        create_virtual_network_phase: WeaveEwVpcPhase,
    ) -> (
        PathBuf,
        Arc<Mutex<RecordedWeaveEwVpcCalls>>,
        Arc<Mutex<RecordedWeaveEwVpcState>>,
        RecordingWeaveServer,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let calls = Arc::new(Mutex::new(RecordedWeaveEwVpcCalls::default()));
        let state = Arc::new(Mutex::new(RecordedWeaveEwVpcState {
            virtual_networks,
            virtual_network_attachments,
            ..Default::default()
        }));
        let service = RecordingNetworkIsolationService {
            state: state.clone(),
            calls: calls.clone(),
            create_virtual_network_phase,
        };
        let task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(NetworkIsolationServiceServer::new(service))
                .serve_with_incoming(UnixListenerStream::new(listener))
                .await
                .unwrap();
        });
        (
            socket_path,
            calls,
            state,
            RecordingWeaveServer {
                _directory: dir,
                task,
            },
        )
    }

    /// Gives the full-agent fixture a private Weave service whose List failure
    /// can be removed between iterations to prove recovery through the main loop.
    pub(crate) fn start_recording_weave_ew_vpc_mock_server_for_agent() -> (
        PathBuf,
        Arc<Mutex<RecordedWeaveEwVpcState>>,
        RecordingWeaveServer,
    ) {
        let (socket, _, state, server) = start_recording_weave_ew_vpc_mock_server_with_state(
            Vec::new(),
            Vec::new(),
            WeaveEwVpcPhase::Ready,
        );
        (socket, state, server)
    }

    /// Runs reconciliation through real socket I/O. Only a request that has
    /// reached the injected stall uses the paused clock; later Lists run normally.
    async fn reconcile_with_injected_rpc_failure(
        socket: &Path,
        config: &rpc::AstraConfig,
        failure: &MockRpcFailure,
    ) -> eyre::Result<rpc::AstraConfigStatus> {
        // Keep the future here so a test timeout drops it instead of detaching
        // a task that could continue mutating the fixture after the test fails.
        let reconcile =
            build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(config));
        tokio::pin!(reconcile);
        if let MockRpcFailure::Stall(entered) = failure {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::select! {
                    () = entered.notified() => Ok(()),
                    result = &mut reconcile => Err(eyre::eyre!(
                        "reconciliation completed before injected stall: {result:?}"
                    )),
                }
            })
            .await??;
            tokio::time::pause();
            tokio::time::advance(std::time::Duration::from_secs(10)).await;
            tokio::time::resume();
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), reconcile).await?
    }

    fn astra_attachment(mac_address: &str, vni: u32) -> rpc::AstraAttachment {
        astra_attachment_with_revision(mac_address, vni, "test-revision")
    }

    fn astra_attachment_with_revision(
        mac_address: &str,
        vni: u32,
        revision: &str,
    ) -> rpc::AstraAttachment {
        rpc::AstraAttachment {
            mac_address: mac_address.to_string(),
            vni,
            subnet_ipv4: "192.0.2.0".to_string(),
            subnet_mask: 24,
            attachment_type: Some(rpc::SpxAttachmentType::Physical as i32),
            attachment_vf: None,
            attachment_ovs: None,
            revision: revision.to_string(),
        }
    }

    // A detached / admin-network NIC as produced by the server: vni 0 with no
    // attachment_type (see crates/api-core/src/handlers/astra.rs). It must not
    // create any virtual network or attachment on the Weave server.
    fn astra_attachment_detached(mac_address: &str, revision: &str) -> rpc::AstraAttachment {
        rpc::AstraAttachment {
            mac_address: mac_address.to_string(),
            vni: 0,
            subnet_ipv4: "192.0.2.0".to_string(),
            subnet_mask: 24,
            attachment_type: None,
            attachment_vf: None,
            attachment_ovs: None,
            revision: revision.to_string(),
        }
    }

    fn weave_ew_vpc_virtual_network(id: &str, vni: u32) -> proto::VirtualNetwork {
        weave_ew_vpc_virtual_network_with_revision(id, vni, "test-revision")
    }

    fn weave_ew_vpc_virtual_network_with_revision(
        id: &str,
        vni: u32,
        revision: &str,
    ) -> proto::VirtualNetwork {
        weave_ew_vpc_virtual_network_with_phase(id, vni, WeaveEwVpcPhase::Ready, revision)
    }

    fn weave_ew_vpc_virtual_network_with_phase(
        id: &str,
        vni: u32,
        phase: WeaveEwVpcPhase,
        revision: &str,
    ) -> proto::VirtualNetwork {
        let mut metadata = weave_ew_vpc_object_metadata(Some(id.to_string()), revision);
        metadata.resource_version = Some("test-resource-version".to_string());
        proto::VirtualNetwork {
            metadata: Some(metadata),
            spec: Some(proto::VirtualNetworkSpec {
                vni,
                subnet_ipv4: Some("192.0.2.0/24".to_string()),
                subnet_ipv6: None,
            }),
            status: Some(proto::VirtualNetworkStatus {
                state: Some(State {
                    phase: phase.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
            }),
        }
    }

    fn weave_ew_vpc_virtual_network_attachment(
        id: &str,
        nic_id: &str,
        vnet_id: &str,
    ) -> proto::VirtualNetworkAttachment {
        weave_ew_vpc_virtual_network_attachment_with_revision(id, nic_id, vnet_id, "test-revision")
    }

    fn astra_attachment_ovs(
        mac_address: &str,
        vni: u32,
        bridge_name: &str,
        revision: &str,
    ) -> rpc::AstraAttachment {
        rpc::AstraAttachment {
            mac_address: mac_address.to_string(),
            vni,
            subnet_ipv4: "192.0.2.0".to_string(),
            subnet_mask: 24,
            attachment_type: Some(rpc::SpxAttachmentType::Ovs as i32),
            attachment_vf: None,
            attachment_ovs: Some(rpc::AstraAttachmentOvs {
                bridge_name: bridge_name.to_string(),
                network_name: None,
            }),
            revision: revision.to_string(),
        }
    }

    fn weave_ew_vpc_virtual_network_attachment_ovs(
        id: &str,
        nic_id: &str,
        vnet_id: &str,
        bridge_name: &str,
        revision: &str,
    ) -> proto::VirtualNetworkAttachment {
        let mut metadata = weave_ew_vpc_object_metadata(Some(id.to_string()), revision);
        metadata.resource_version = Some("test-resource-version".to_string());
        proto::VirtualNetworkAttachment {
            metadata: Some(metadata),
            spec: Some(proto::VirtualNetworkAttachmentSpec {
                vnet_id: vnet_id.to_string(),
                nic_id: nic_id.to_string(),
                attachment_type: proto::AttachmentType::Ovs.into(),
                attachment_pf: None,
                attachment_vf: None,
                attachment_ovs: Some(proto::AttachmentOvs {
                    bridge_name: bridge_name.to_string(),
                    ovn_network_name: None,
                }),
            }),
            status: Some(proto::VirtualNetworkAttachmentStatus {
                state: Some(State {
                    phase: WeaveEwVpcPhase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
                host_ipv4: None,
                host_ipv6: None,
            }),
        }
    }

    fn weave_ew_vpc_virtual_network_attachment_with_revision(
        id: &str,
        nic_id: &str,
        vnet_id: &str,
        revision: &str,
    ) -> proto::VirtualNetworkAttachment {
        let mut metadata = weave_ew_vpc_object_metadata(Some(id.to_string()), revision);
        metadata.resource_version = Some("test-resource-version".to_string());
        proto::VirtualNetworkAttachment {
            metadata: Some(metadata),
            spec: Some(proto::VirtualNetworkAttachmentSpec {
                vnet_id: vnet_id.to_string(),
                nic_id: nic_id.to_string(),
                attachment_type: proto::AttachmentType::Pf.into(),
                attachment_pf: Some(proto::AttachmentPf {
                    pf_id: nic_id.to_string(),
                }),
                attachment_vf: None,
                attachment_ovs: None,
            }),
            status: Some(proto::VirtualNetworkAttachmentStatus {
                state: Some(State {
                    phase: WeaveEwVpcPhase::Ready.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
                host_ipv4: None,
                host_ipv6: None,
            }),
        }
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_none_returns_empty_status()
    -> eyre::Result<()> {
        let networks = vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)];
        let attachments = vec![weave_ew_vpc_virtual_network_attachment(
            "retained",
            "02:aa:bb:cc:dd:ee",
            "astra-weave-vni-100",
        )];
        let (socket, calls, state, server) = start_recording_weave_ew_vpc_mock_server_with_state(
            networks.clone(),
            attachments.clone(),
            WeaveEwVpcPhase::Ready,
        );
        let status = TEST_WEAVE_EW_VPC_SOCKET_PATH
            .scope(
                socket.to_str().unwrap().to_string(),
                build_notify_weave_ew_vpc_astra_config(None),
            )
            .await?;

        assert!(status.astra_attachments_status.is_empty());
        assert_eq!(status.is_attachment_status_present, Some(false));
        assert_eq!(status.is_maintenance_status_present, Some(false));
        {
            let calls = calls.lock().await;
            assert_eq!(calls.list_virtual_networks, 0);
            assert_eq!(calls.list_virtual_network_attachments, 0);
            assert!(calls.create_virtual_networks.is_empty());
            assert!(calls.update_virtual_networks.is_empty());
            assert!(calls.delete_virtual_networks.is_empty());
            assert!(calls.create_virtual_network_attachments.is_empty());
            assert!(calls.update_virtual_network_attachments.is_empty());
            assert!(calls.delete_virtual_network_attachments.is_empty());
            let state = state.lock().await;
            assert_eq!(state.virtual_networks, networks);
            assert_eq!(state.virtual_network_attachments, attachments);
        }
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_adds_missing_vni_and_attachment()
    -> eyre::Result<()> {
        let (socket_path, calls, server) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new());
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            AstraPhase::PhaseReady as i32
        );
        assert_eq!(calls.list_virtual_networks, 4);
        assert_eq!(calls.list_virtual_network_attachments, 3);
        assert_eq!(calls.create_virtual_networks.len(), 1);
        assert!(calls.update_virtual_networks.is_empty());
        assert_eq!(
            calls.create_virtual_networks[0].spec.as_ref().unwrap().vni,
            100
        );
        assert_eq!(calls.create_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.create_virtual_network_attachments[0]
                .metadata
                .as_ref()
                .unwrap()
                .user_data
                .get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY),
            Some(&"test-revision".to_string())
        );
        assert_eq!(
            calls.create_virtual_network_attachments[0]
                .spec
                .as_ref()
                .unwrap()
                .vnet_id,
            "astra-weave-vni-100"
        );
        assert!(
            calls.create_virtual_network_attachments[0]
                .spec
                .as_ref()
                .unwrap()
                .attachment_pf
                .is_some()
        );
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.delete_virtual_network_attachments.is_empty());

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_converged_state_skips_reconcile()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
            vec![weave_ew_vpc_virtual_network_attachment(
                "matching-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            rpc::AstraPhase::PhaseReady as i32
        );
        assert_eq!(calls.list_virtual_network_attachments, 1);
        assert_eq!(calls.list_virtual_networks, 1);
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.delete_virtual_network_attachments.is_empty());

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_converged_state_includes_detached_nic()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
            vec![weave_ew_vpc_virtual_network_attachment(
                "matching-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment_detached("02:aa:bb:cc:dd:ff", "test-revision"),
            ],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 2);
        let attached = status
            .astra_attachments_status
            .iter()
            .find(|entry| entry.mac_address == "02:aa:bb:cc:dd:ee")
            .unwrap();
        assert_eq!(
            attached.status.as_ref().unwrap().phase,
            rpc::AstraPhase::PhaseReady as i32
        );
        let detached = status
            .astra_attachments_status
            .iter()
            .find(|entry| entry.mac_address == "02:aa:bb:cc:dd:ff")
            .unwrap();
        assert_eq!(detached.vni, 0);
        assert!(detached.attachment_type.is_none());
        assert_eq!(
            detached.status.as_ref().unwrap().phase,
            rpc::AstraPhase::PhaseReady as i32
        );
        assert_eq!(calls.list_virtual_network_attachments, 1);
        assert_eq!(calls.list_virtual_networks, 1);
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.delete_virtual_network_attachments.is_empty());

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_revision_bump_does_not_recreate_matching_topology()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network_with_revision(
                "astra-weave-vni-100",
                100,
                "old-revision",
            )],
            vec![weave_ew_vpc_virtual_network_attachment_with_revision(
                "matching-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
                "old-revision",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.create_virtual_networks.is_empty());
        assert_eq!(calls.update_virtual_networks.len(), 1);
        let metadata = calls.update_virtual_networks[0].metadata.as_ref().unwrap();
        assert_eq!(metadata.id.as_deref(), Some("astra-weave-vni-100"));
        assert_eq!(
            metadata.resource_version.as_deref(),
            Some("test-resource-version")
        );
        assert_eq!(
            metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY),
            Some(&"test-revision".to_string())
        );
        assert!(calls.delete_virtual_network_attachments.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert_eq!(calls.update_virtual_network_attachments.len(), 1);
        let metadata = calls.update_virtual_network_attachments[0]
            .metadata
            .as_ref()
            .unwrap();
        assert_eq!(metadata.id.as_deref(), Some("matching-attachment"));
        assert_eq!(
            metadata.resource_version.as_deref(),
            Some("test-resource-version")
        );
        assert_eq!(
            metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY),
            Some(&"test-revision".to_string())
        );

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_extra_attachment_is_reconciled()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
            vec![
                weave_ew_vpc_virtual_network_attachment(
                    "matching-attachment",
                    "02:aa:bb:cc:dd:ee",
                    "astra-weave-vni-100",
                ),
                weave_ew_vpc_virtual_network_attachment(
                    "stale-extra-attachment",
                    "02:aa:bb:cc:dd:ff",
                    "astra-weave-vni-100",
                ),
            ],
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            AstraPhase::PhaseReady as i32
        );
        assert_eq!(calls.list_virtual_network_attachments, 3);
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "stale-extra-attachment"
        );
        assert!(calls.create_virtual_network_attachments.is_empty());

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_creates_shared_vni_once()
    -> eyre::Result<()> {
        let (socket_path, calls, server) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new());
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment("02:aa:bb:cc:dd:ff", 100),
            ],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 2);
        assert_eq!(calls.create_virtual_networks.len(), 1);
        assert_eq!(
            calls.create_virtual_networks[0].spec.as_ref().unwrap().vni,
            100
        );
        assert_eq!(calls.create_virtual_network_attachments.len(), 2);
        assert!(
            calls
                .create_virtual_network_attachments
                .iter()
                .all(|request| request.spec.as_ref().unwrap().vnet_id == "astra-weave-vni-100")
        );

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_marks_pending_vni_seen()
    -> eyre::Result<()> {
        let (socket_path, calls, server) =
            start_recording_weave_ew_vpc_mock_server_with_create_phase(
                Vec::new(),
                Vec::new(),
                WeaveEwVpcPhase::Pending,
            );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment("02:aa:bb:cc:dd:ff", 100),
            ],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 2);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            rpc::AstraPhase::PhasePending as i32
        );
        assert_eq!(calls.create_virtual_networks.len(), 1);
        assert_eq!(
            calls.create_virtual_networks[0].spec.as_ref().unwrap().vni,
            100
        );

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_marks_existing_pending_vni_not_ready()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network_with_phase(
                "astra-weave-vni-100",
                100,
                WeaveEwVpcPhase::Pending,
                "test-revision",
            )],
            Vec::new(),
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment("02:aa:bb:cc:dd:ff", 100),
            ],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 2);
        assert!(status.astra_attachments_status.iter().all(|attachment| {
            attachment
                .status
                .as_ref()
                .is_some_and(|status| status.phase == rpc::AstraPhase::PhasePending as i32)
        }));
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_deletes_stale_vni_and_attachment()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("stale-vni", 300)],
            vec![weave_ew_vpc_virtual_network_attachment(
                "stale-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-300",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: Vec::new(),
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert!(status.astra_attachments_status.is_empty());
        assert_eq!(calls.list_virtual_networks, 4);
        assert_eq!(calls.list_virtual_network_attachments, 3);
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert_eq!(calls.delete_virtual_networks.len(), 1);
        assert_eq!(calls.delete_virtual_networks[0].id, "stale-vni");
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "stale-attachment"
        );

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_moves_attachment_to_new_vni()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![
                weave_ew_vpc_virtual_network("old-vni", 100),
                weave_ew_vpc_virtual_network("astra-weave-vni-200", 200),
            ],
            vec![weave_ew_vpc_virtual_network_attachment(
                "old-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 200)],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert!(calls.create_virtual_networks.is_empty());
        assert_eq!(calls.delete_virtual_networks.len(), 1);
        assert_eq!(calls.delete_virtual_networks[0].id, "old-vni");
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "old-attachment"
        );
        assert_eq!(calls.create_virtual_network_attachments.len(), 1);
        let create_attachment_spec = calls.create_virtual_network_attachments[0]
            .spec
            .as_ref()
            .unwrap();
        assert_eq!(create_attachment_spec.nic_id, "02:aa:bb:cc:dd:ee");
        assert_eq!(create_attachment_spec.vnet_id, "astra-weave-vni-200");
        assert!(create_attachment_spec.attachment_pf.is_some());

        server.shutdown().await?;
        Ok(())
    }

    // Changing an OVS attachment's bridge (br-old -> br-new) while keeping the
    // same NIC and VNI cannot be applied in place: Weave has to delete the old
    // attachment and create a replacement bound to the new bridge. A revision
    // bump alone would leave the dataplane on the stale bridge.
    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_recreates_ovs_attachment_on_bridge_change()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network_with_revision(
                "astra-weave-vni-100",
                100,
                "revision-1",
            )],
            vec![weave_ew_vpc_virtual_network_attachment_ovs(
                "old-ovs-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
                "br-old",
                "revision-1",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        // Same NIC and VNI, new revision, only the OVS bridge changes.
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment_ovs(
                "02:aa:bb:cc:dd:ee",
                100,
                "br-new",
                "revision-2",
            )],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        // The VNI is unchanged, so the virtual network is reused (revision bump),
        // never recreated.
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.delete_virtual_networks.is_empty());

        // The stale-bridge attachment is deleted and a replacement is created;
        // the in-place update path must not run for a bridge change.
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "old-ovs-attachment"
        );
        assert!(calls.update_virtual_network_attachments.is_empty());
        assert_eq!(calls.create_virtual_network_attachments.len(), 1);
        let create_attachment_spec = calls.create_virtual_network_attachments[0]
            .spec
            .as_ref()
            .unwrap();
        assert_eq!(create_attachment_spec.nic_id, "02:aa:bb:cc:dd:ee");
        assert_eq!(create_attachment_spec.vnet_id, "astra-weave-vni-100");
        assert_eq!(
            create_attachment_spec.attachment_type,
            proto::AttachmentType::Ovs as i32
        );
        assert_eq!(
            create_attachment_spec
                .attachment_ovs
                .as_ref()
                .unwrap()
                .bridge_name,
            "br-new"
        );

        // After a successful replacement the attachment is reported Ready.
        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            AstraPhase::PhaseReady as i32
        );

        server.shutdown().await?;
        Ok(())
    }

    // Switching an existing OVS attachment to Physical on the same NIC and VNI
    // changes the dataplane binding, so the stale OVS attachment must be deleted
    // and a Physical replacement created rather than the revision bumped in place
    // (which would leave the OVS bridge installed).
    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_recreates_attachment_on_ovs_to_physical()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network_with_revision(
                "astra-weave-vni-100",
                100,
                "revision-1",
            )],
            vec![weave_ew_vpc_virtual_network_attachment_ovs(
                "old-ovs-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
                "br-old",
                "revision-1",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        // Same NIC and VNI, new revision, attachment type changes OVS -> Physical.
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment_with_revision(
                "02:aa:bb:cc:dd:ee",
                100,
                "revision-2",
            )],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        // The VNI is unchanged, so the virtual network is reused, not recreated.
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.delete_virtual_networks.is_empty());

        // The stale OVS attachment is deleted and a Physical replacement created;
        // the in-place update path must not run for a type change.
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "old-ovs-attachment"
        );
        assert!(calls.update_virtual_network_attachments.is_empty());
        assert_eq!(calls.create_virtual_network_attachments.len(), 1);
        let create_attachment_spec = calls.create_virtual_network_attachments[0]
            .spec
            .as_ref()
            .unwrap();
        assert_eq!(create_attachment_spec.nic_id, "02:aa:bb:cc:dd:ee");
        assert_eq!(create_attachment_spec.vnet_id, "astra-weave-vni-100");
        assert_eq!(
            create_attachment_spec.attachment_type,
            proto::AttachmentType::Pf as i32
        );
        assert!(create_attachment_spec.attachment_pf.is_some());
        assert!(create_attachment_spec.attachment_ovs.is_none());

        // After a successful replacement the attachment is reported Ready.
        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            AstraPhase::PhaseReady as i32
        );

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_errors_on_conflicting_virtual_network_id()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("matching-vni", 100)],
            Vec::new(),
        );
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        let astra_status = status.astra_attachments_status[0].status.as_ref().unwrap();
        assert_eq!(astra_status.phase, rpc::AstraPhase::PhaseError as i32);
        assert_eq!(
            astra_status.reason,
            "Conflicting DOCA Weave virtual network ID"
        );
        assert!(astra_status.message.contains("matching-vni"));
        assert!(astra_status.message.contains("astra-weave-vni-100"));
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_detached_nic_creates_nothing()
    -> eyre::Result<()> {
        let (socket_path, calls, server) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new());
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment_detached("02:aa:bb:cc:dd:ee", "revision-1")],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        // The detached NIC (vni 0) is reported Ready but produces no virtual
        // network or attachment on the Weave server (which requires vni >= 1).
        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(status.astra_attachments_status[0].vni, 0);
        assert!(status.astra_attachments_status[0].attachment_type.is_none());
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            AstraPhase::PhaseReady as i32
        );
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.delete_virtual_network_attachments.is_empty());

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_detaches_existing_attachment()
    -> eyre::Result<()> {
        let (socket_path, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network_with_revision(
                "astra-weave-vni-100",
                100,
                "revision-1",
            )],
            vec![weave_ew_vpc_virtual_network_attachment_with_revision(
                "stale-attachment-for-detached-mac",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
                "revision-1",
            )],
        );
        let socket_path = socket_path.to_str().unwrap();
        // The NIC previously attached to VNI 100 is now detached (VNI 0).
        // Its remaining service resources require cleanup before Ready.
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment_detached("02:aa:bb:cc:dd:ee", "revision-2")],
        };

        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            AstraPhase::PhaseReady as i32
        );
        assert_eq!(status.astra_attachments_status[0].vni, 0);
        assert!(status.astra_attachments_status[0].attachment_type.is_none());

        // No new virtual network or attachment is created for the detached NIC.
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());

        // The stale attachment is detached and the now-unreferenced virtual
        // network is deleted.
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "stale-attachment-for-detached-mac"
        );
        assert_eq!(calls.delete_virtual_networks.len(), 1);
        assert_eq!(calls.delete_virtual_networks[0].id, "astra-weave-vni-100");

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_processes_full_config_sequence()
    -> eyre::Result<()> {
        let (socket_path, calls, server) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new());
        let socket_path = socket_path.to_str().unwrap();

        let config = |attachments: Vec<(&str, u32)>, revision: &str| rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: attachments
                .into_iter()
                .map(|(mac_address, vni)| {
                    astra_attachment_with_revision(mac_address, vni, revision)
                })
                .collect(),
        };

        let run_update = |astra_config: rpc::AstraConfig| async move {
            build_notify_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await
        };

        // Step 1: initial config on empty server.
        run_update(config(vec![("aa:bb:cc:dd:ee:10", 100)], "revision-1")).await?;
        {
            let mut calls = calls.lock().await;
            assert_eq!(calls.create_virtual_networks.len(), 1);
            assert_eq!(
                calls.create_virtual_networks[0].spec.as_ref().unwrap().vni,
                100
            );
            assert_eq!(
                calls.create_virtual_networks[0]
                    .metadata
                    .as_ref()
                    .unwrap()
                    .user_data
                    .get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY),
                Some(&"revision-1".to_string())
            );
            assert_eq!(calls.create_virtual_network_attachments.len(), 1);
            assert_eq!(
                calls.create_virtual_network_attachments[0]
                    .spec
                    .as_ref()
                    .unwrap()
                    .nic_id,
                "aa:bb:cc:dd:ee:10"
            );
            assert!(calls.delete_virtual_networks.is_empty());
            assert!(calls.delete_virtual_network_attachments.is_empty());
            *calls = RecordedWeaveEwVpcCalls::default();
        }

        // Step 2: add a second attachment; revision bump alone does not recreate existing objects.
        run_update(config(
            vec![("aa:bb:cc:dd:ee:10", 100), ("aa:bb:cc:dd:ee:20", 200)],
            "revision-2",
        ))
        .await?;
        {
            let mut calls = calls.lock().await;
            assert!(calls.delete_virtual_networks.is_empty());
            assert_eq!(calls.create_virtual_networks.len(), 1);
            assert_eq!(
                calls.create_virtual_networks[0].spec.as_ref().unwrap().vni,
                200
            );
            assert!(calls.create_virtual_networks.iter().all(|request| {
                request.metadata.as_ref().and_then(|metadata| {
                    metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY)
                }) == Some(&"revision-2".to_string())
            }));
            assert!(calls.delete_virtual_network_attachments.is_empty());
            assert_eq!(calls.create_virtual_network_attachments.len(), 1);
            assert_eq!(
                calls.create_virtual_network_attachments[0]
                    .spec
                    .as_ref()
                    .unwrap()
                    .nic_id,
                "aa:bb:cc:dd:ee:20"
            );
            assert!(
                calls
                    .create_virtual_network_attachments
                    .iter()
                    .all(|request| {
                        request.metadata.as_ref().and_then(|metadata| {
                            metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY)
                        }) == Some(&"revision-2".to_string())
                    })
            );
            *calls = RecordedWeaveEwVpcCalls::default();
        }

        // Step 3: add a third attachment under a new revision.
        run_update(config(
            vec![
                ("aa:bb:cc:dd:ee:10", 100),
                ("aa:bb:cc:dd:ee:20", 200),
                ("aa:bb:cc:dd:ee:30", 300),
            ],
            "revision-3",
        ))
        .await?;
        {
            let mut calls = calls.lock().await;
            assert!(calls.delete_virtual_networks.is_empty());
            assert_eq!(calls.create_virtual_networks.len(), 1);
            assert_eq!(
                calls.create_virtual_networks[0].spec.as_ref().unwrap().vni,
                300
            );
            assert!(calls.delete_virtual_network_attachments.is_empty());
            assert_eq!(calls.create_virtual_network_attachments.len(), 1);
            assert_eq!(
                calls.create_virtual_network_attachments[0]
                    .spec
                    .as_ref()
                    .unwrap()
                    .nic_id,
                "aa:bb:cc:dd:ee:30"
            );
            assert!(
                calls
                    .create_virtual_network_attachments
                    .iter()
                    .all(|request| {
                        request.metadata.as_ref().and_then(|metadata| {
                            metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY)
                        }) == Some(&"revision-3".to_string())
                    })
            );
            *calls = RecordedWeaveEwVpcCalls::default();
        }

        // Step 4: move one attachment to a new VNI under a new revision.
        run_update(config(
            vec![
                ("aa:bb:cc:dd:ee:10", 100),
                ("aa:bb:cc:dd:ee:20", 400),
                ("aa:bb:cc:dd:ee:30", 300),
            ],
            "revision-4",
        ))
        .await?;
        {
            let mut calls = calls.lock().await;
            assert_eq!(calls.delete_virtual_networks.len(), 1);
            assert!(
                calls
                    .delete_virtual_networks
                    .iter()
                    .any(|request| request.id == "astra-weave-vni-200")
            );
            assert_eq!(calls.create_virtual_networks.len(), 1);
            assert_eq!(
                calls.create_virtual_networks[0].spec.as_ref().unwrap().vni,
                400
            );
            assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
            assert!(
                calls.delete_virtual_network_attachments[0]
                    .id
                    .contains("aa:bb:cc:dd:ee:20")
            );
            assert_eq!(calls.create_virtual_network_attachments.len(), 1);
            let moved_attachment = calls.create_virtual_network_attachments[0]
                .spec
                .as_ref()
                .unwrap();
            assert_eq!(moved_attachment.nic_id, "aa:bb:cc:dd:ee:20");
            assert_eq!(moved_attachment.vnet_id, "astra-weave-vni-400");
            assert!(moved_attachment.attachment_pf.is_some());
            *calls = RecordedWeaveEwVpcCalls::default();
        }

        // Step 5: remove one attachment under a new revision.
        run_update(config(
            vec![("aa:bb:cc:dd:ee:20", 400), ("aa:bb:cc:dd:ee:30", 300)],
            "revision-5",
        ))
        .await?;
        {
            let calls = calls.lock().await;
            assert!(calls.create_virtual_networks.is_empty());
            assert!(calls.create_virtual_network_attachments.is_empty());
            assert_eq!(calls.delete_virtual_networks.len(), 1);
            assert!(
                calls
                    .delete_virtual_networks
                    .iter()
                    .any(|request| request.id == "astra-weave-vni-100")
            );
            assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
            assert!(
                calls
                    .delete_virtual_network_attachments
                    .iter()
                    .any(|request| request.id.contains("aa:bb:cc:dd:ee:10"))
            );
        }

        server.shutdown().await?;
        Ok(())
    }

    /// A detached NIC stays Pending while either stale resource remains visible.
    /// Another NIC can keep using the shared VNI throughout that cleanup.
    #[tokio::test]
    async fn cleanup_waits_for_service_disappearance_and_retains_shared_vni() -> eyre::Result<()> {
        #[derive(Clone, Copy, Debug)]
        enum DeletionOrder {
            Attachments,
            Network,
        }

        for order in [DeletionOrder::Attachments, DeletionOrder::Network] {
            let (socket, calls, state, server) =
                start_recording_weave_ew_vpc_mock_server_with_state(
                    vec![
                        weave_ew_vpc_virtual_network("astra-weave-vni-100", 100),
                        weave_ew_vpc_virtual_network("astra-weave-vni-200", 200),
                    ],
                    vec![
                        weave_ew_vpc_virtual_network_attachment(
                            "retained",
                            "02:aa:bb:cc:dd:ee",
                            "astra-weave-vni-100",
                        ),
                        weave_ew_vpc_virtual_network_attachment(
                            "detaching",
                            "02:aa:bb:cc:dd:ff",
                            "astra-weave-vni-100",
                        ),
                        weave_ew_vpc_virtual_network_attachment(
                            "stale",
                            "02:aa:bb:cc:dd:00",
                            "astra-weave-vni-200",
                        ),
                    ],
                    WeaveEwVpcPhase::Ready,
                );
            state.lock().await.retain_deleted_resources = true;
            let config = rpc::AstraConfig {
                astra_attachments: vec![
                    astra_attachment("02:aa:bb:cc:dd:ee", 100),
                    astra_attachment_detached("02:aa:bb:cc:dd:ff", "test-revision"),
                ],
                ..Default::default()
            };

            // Delete succeeds before either kind of resource disappears.
            let status =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await?;
            assert_eq!(status.astra_attachments_status.len(), 2, "{order:?}");
            for attachment in &status.astra_attachments_status {
                let status = attachment.status.as_ref().unwrap();
                assert_eq!(status.phase, AstraPhase::PhasePending as i32, "{order:?}");
                assert_eq!(status.reason, "DOCA Weave state has not converged");
                assert!(status.message.contains("virtual network count mismatch"));
            }
            {
                let calls = calls.lock().await;
                assert_eq!(
                    calls
                        .delete_virtual_network_attachments
                        .iter()
                        .map(|request| request.id.as_str())
                        .collect::<Vec<_>>(),
                    ["detaching", "stale"],
                    "{order:?}"
                );
                assert_eq!(
                    calls
                        .delete_virtual_networks
                        .iter()
                        .map(|request| request.id.as_str())
                        .collect::<Vec<_>>(),
                    ["astra-weave-vni-200"],
                    "{order:?}"
                );
            }

            // Leave exactly one cleanup boundary outstanding. When the network goes,
            // the extra attachment still belongs to the retained shared VNI.
            let expected_message = {
                let mut state = state.lock().await;
                match order {
                    DeletionOrder::Attachments => {
                        state.virtual_network_attachments.retain(|attachment| {
                            attachment.metadata.as_ref().unwrap().id.as_deref() == Some("retained")
                        });
                        "virtual network count mismatch"
                    }
                    DeletionOrder::Network => {
                        state
                            .virtual_networks
                            .retain(|network| network.spec.as_ref().unwrap().vni == 100);
                        state.virtual_network_attachments.retain(|attachment| {
                            attachment.metadata.as_ref().unwrap().id.as_deref() != Some("stale")
                        });
                        "attachment count mismatch"
                    }
                }
            };
            let status =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await?;
            assert_eq!(status.astra_attachments_status.len(), 2, "{order:?}");
            for attachment in &status.astra_attachments_status {
                let status = attachment.status.as_ref().unwrap();
                assert_eq!(status.phase, AstraPhase::PhasePending as i32, "{order:?}");
                assert_eq!(status.reason, "DOCA Weave state has not converged");
                assert!(
                    status.message.contains(expected_message),
                    "{order:?}: {status:?}"
                );
            }

            {
                let mut calls = calls.lock().await;
                match order {
                    DeletionOrder::Attachments => {
                        assert_eq!(calls.delete_virtual_networks.len(), 2);
                        assert!(
                            calls
                                .delete_virtual_networks
                                .iter()
                                .all(|request| request.id == "astra-weave-vni-200")
                        );
                        assert_eq!(calls.delete_virtual_network_attachments.len(), 2);
                    }
                    DeletionOrder::Network => {
                        assert_eq!(calls.delete_virtual_networks.len(), 1);
                        assert_eq!(calls.delete_virtual_network_attachments.len(), 3);
                        assert_eq!(calls.delete_virtual_network_attachments[2].id, "detaching");
                    }
                }
                assert!(calls.create_virtual_networks.is_empty());
                assert!(calls.create_virtual_network_attachments.is_empty());
                *calls = RecordedWeaveEwVpcCalls::default();
            }

            {
                let mut state = state.lock().await;
                state
                    .virtual_networks
                    .retain(|network| network.spec.as_ref().unwrap().vni == 100);
                state.virtual_network_attachments.retain(|attachment| {
                    attachment.metadata.as_ref().unwrap().id.as_deref() == Some("retained")
                });
            }
            let status =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await?;
            assert_eq!(status.astra_attachments_status.len(), 2, "{order:?}");
            assert!(
                status.astra_attachments_status.iter().all(|attachment| {
                    attachment.status.as_ref().unwrap().phase == AstraPhase::PhaseReady as i32
                }),
                "{order:?}"
            );
            let calls = calls.lock().await;
            assert!(calls.create_virtual_networks.is_empty());
            assert!(calls.update_virtual_networks.is_empty());
            assert!(calls.delete_virtual_networks.is_empty());
            assert!(calls.create_virtual_network_attachments.is_empty());
            assert!(calls.update_virtual_network_attachments.is_empty());
            assert!(calls.delete_virtual_network_attachments.is_empty());
            server.shutdown().await?;
        }
        Ok(())
    }

    /// Matching attachment revisions must not hide a stale network or a wrong
    /// PF binding; only the changed binding needs replacement.
    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_repairs_revision_and_binding_drift()
    -> eyre::Result<()> {
        let mut wrong_binding = weave_ew_vpc_virtual_network_attachment(
            "wrong-binding",
            "02:aa:bb:cc:dd:00",
            "astra-weave-vni-100",
        );
        // Keep NIC and VNI matching so the full PF comparison has to find
        // the drift; matching only NIC and VNI would reuse this attachment.
        wrong_binding
            .spec
            .as_mut()
            .unwrap()
            .attachment_pf
            .as_mut()
            .unwrap()
            .pf_id = "02:00:00:00:00:00".to_string();
        let (socket, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![
                weave_ew_vpc_virtual_network_with_revision(
                    "astra-weave-vni-100",
                    100,
                    "old-revision",
                ),
                weave_ew_vpc_virtual_network("astra-weave-vni-200", 200),
            ],
            vec![
                weave_ew_vpc_virtual_network_attachment(
                    "matching",
                    "02:aa:bb:cc:dd:ee",
                    "astra-weave-vni-100",
                ),
                weave_ew_vpc_virtual_network_attachment_with_revision(
                    "old-revision",
                    "02:aa:bb:cc:dd:ff",
                    "astra-weave-vni-100",
                    "old-revision",
                ),
                wrong_binding,
            ],
        );
        let config = rpc::AstraConfig {
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment("02:aa:bb:cc:dd:ff", 100),
                astra_attachment("02:aa:bb:cc:dd:00", 100),
            ],
            ..Default::default()
        };
        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                .await?;
        assert_eq!(status.astra_attachments_status.len(), 3);
        assert!(status.astra_attachments_status.iter().all(|attachment| {
            attachment.status.as_ref().unwrap().phase == AstraPhase::PhaseReady as i32
        }));
        let calls = calls.lock().await;
        assert_eq!(calls.update_virtual_networks.len(), 1);
        assert_eq!(calls.update_virtual_network_attachments.len(), 2);
        assert_eq!(calls.delete_virtual_networks.len(), 1);
        assert_eq!(calls.delete_virtual_networks[0].id, "astra-weave-vni-200");
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "wrong-binding"
        );
        assert_eq!(calls.create_virtual_network_attachments.len(), 1);
        server.shutdown().await?;
        Ok(())
    }

    /// Resource counts and revisions are insufficient: each observed resource
    /// must also have the desired identity, binding, and Ready state.
    #[test]
    fn complete_observation_checks_resource_state_identity_and_binding() {
        use carbide_test_support::Outcome::{FailsWith, Yields};
        use carbide_test_support::{Case, check_cases};

        struct Resources {
            desired: rpc::AstraConfig,
            networks: Vec<proto::VirtualNetwork>,
            attachments: Vec<proto::VirtualNetworkAttachment>,
        }

        impl Resources {
            /// Aligns the OVS binding except for its optional network name, so
            /// each case tests that field without an unrelated PF/type mismatch.
            fn set_ovs_network_name(&mut self, observed_name: &str) {
                self.desired.astra_attachments[1] =
                    astra_attachment_ovs("02:aa:bb:cc:dd:ff", 100, "br-test", "test-revision");
                self.desired.astra_attachments[1]
                    .attachment_ovs
                    .as_mut()
                    .unwrap()
                    .network_name = Some("tenant-network".to_string());
                self.attachments[1] = weave_ew_vpc_virtual_network_attachment_ovs(
                    "second-attachment",
                    "02:aa:bb:cc:dd:ff",
                    "astra-weave-vni-100",
                    "br-test",
                    "test-revision",
                );
                self.attachments[1]
                    .spec
                    .as_mut()
                    .unwrap()
                    .attachment_ovs
                    .as_mut()
                    .unwrap()
                    .ovn_network_name = Some(observed_name.to_string());
            }
        }

        type ResourceCase = Case<fn(&mut Resources), (), String>;
        let cases: [ResourceCase; 24] = [
            Case {
                scenario: "matching resources",
                input: |_| {},
                expect: Yields(()),
            },
            Case {
                scenario: "uppercase MAC addresses",
                input: |resources| {
                    // Case changes must be accepted in both the NIC identity
                    // and its PF binding without weakening other spec checks.
                    let spec = resources.attachments[1].spec.as_mut().unwrap();
                    spec.nic_id.make_ascii_uppercase();
                    spec.attachment_pf
                        .as_mut()
                        .unwrap()
                        .pf_id
                        .make_ascii_uppercase();
                },
                expect: Yields(()),
            },
            Case {
                scenario: "matching OVN network name",
                input: |resources| resources.set_ovs_network_name("tenant-network"),
                expect: Yields(()),
            },
            Case {
                scenario: "changed OVN network name",
                input: |resources| resources.set_ovs_network_name("other-network"),
                expect: FailsWith(
                    "missing matching DOCA weave attachment for MAC 02:aa:bb:cc:dd:ff VNI 100"
                        .to_string(),
                ),
            },
            Case {
                scenario: "network pending",
                input: |resources| {
                    resources.networks[0]
                        .status
                        .as_mut()
                        .unwrap()
                        .state
                        .as_mut()
                        .unwrap()
                        .phase = WeaveEwVpcPhase::Pending.into();
                },
                expect: FailsWith("virtual network astra-weave-vni-100 is not ready".to_string()),
            },
            Case {
                scenario: "attachment deleting",
                input: |resources| {
                    resources.attachments[1]
                        .status
                        .as_mut()
                        .unwrap()
                        .state
                        .as_mut()
                        .unwrap()
                        .phase = WeaveEwVpcPhase::Deleting.into();
                },
                expect: FailsWith("attachment second-attachment is not ready".to_string()),
            },
            Case {
                scenario: "network deletion timestamp",
                input: |resources| {
                    resources.networks[0]
                        .metadata
                        .as_mut()
                        .unwrap()
                        .deletion_timestamp = Some(Default::default());
                },
                expect: FailsWith(
                    "virtual network astra-weave-vni-100 has an unexpected ID or is deleting"
                        .to_string(),
                ),
            },
            Case {
                scenario: "attachment deletion timestamp",
                input: |resources| {
                    resources.attachments[1]
                        .metadata
                        .as_mut()
                        .unwrap()
                        .deletion_timestamp = Some(Default::default());
                },
                expect: FailsWith("attachment second-attachment is deleting".to_string()),
            },
            Case {
                scenario: "missing network metadata",
                input: |resources| resources.networks[0].metadata = None,
                expect: FailsWith(
                    "virtual network astra-weave-vni-100 is missing metadata".to_string(),
                ),
            },
            Case {
                scenario: "missing attachment metadata",
                input: |resources| resources.attachments[1].metadata = None,
                expect: FailsWith("DOCA weave attachment is missing metadata".to_string()),
            },
            Case {
                scenario: "missing attachment ID",
                input: |resources| resources.attachments[1].metadata.as_mut().unwrap().id = None,
                expect: FailsWith("DOCA weave attachment is missing ID".to_string()),
            },
            Case {
                scenario: "empty attachment ID",
                input: |resources| {
                    resources.attachments[1]
                        .metadata
                        .as_mut()
                        .unwrap()
                        .id
                        .as_mut()
                        .unwrap()
                        .clear()
                },
                expect: FailsWith("DOCA weave attachment is missing ID".to_string()),
            },
            Case {
                scenario: "duplicate attachment ID",
                input: |resources| {
                    resources.attachments[1].metadata.as_mut().unwrap().id =
                        Some("first-attachment".to_string());
                },
                expect: FailsWith(
                    "duplicate DOCA weave attachment ID first-attachment".to_string(),
                ),
            },
            Case {
                scenario: "missing network spec",
                input: |resources| resources.networks[0].spec = None,
                expect: FailsWith("DOCA weave virtual network is missing spec".to_string()),
            },
            Case {
                scenario: "missing attachment state",
                input: |resources| resources.attachments[1].status.as_mut().unwrap().state = None,
                expect: FailsWith("attachment second-attachment is not ready".to_string()),
            },
            Case {
                scenario: "wrong subnet",
                input: |resources| {
                    resources.networks[0].spec.as_mut().unwrap().subnet_ipv4 =
                        Some("198.51.100.0/24".to_string());
                },
                expect: FailsWith(
                    "virtual network astra-weave-vni-100 subnet does not match".to_string(),
                ),
            },
            Case {
                scenario: "unexpected IPv6 subnet",
                input: |resources| {
                    resources.networks[0].spec.as_mut().unwrap().subnet_ipv6 =
                        Some("2001:db8::/64".to_string());
                },
                expect: FailsWith(
                    "virtual network astra-weave-vni-100 subnet does not match".to_string(),
                ),
            },
            Case {
                scenario: "network revision",
                input: |resources| {
                    resources.networks[0]
                        .metadata
                        .as_mut()
                        .unwrap()
                        .user_data
                        .insert(
                            WEAVE_EW_VPC_REVISION_USER_DATA_KEY.to_string(),
                            "old".to_string(),
                        );
                },
                expect: FailsWith(
                    "virtual network astra-weave-vni-100 revision does not match".to_string(),
                ),
            },
            Case {
                scenario: "second attachment revision",
                input: |resources| {
                    resources.attachments[1]
                        .metadata
                        .as_mut()
                        .unwrap()
                        .user_data
                        .insert(
                            WEAVE_EW_VPC_REVISION_USER_DATA_KEY.to_string(),
                            "old".to_string(),
                        );
                },
                expect: FailsWith(
                    "attachment second-attachment revision does not match".to_string(),
                ),
            },
            Case {
                scenario: "changed PF binding",
                input: |resources| {
                    resources.attachments[1]
                        .spec
                        .as_mut()
                        .unwrap()
                        .attachment_pf
                        .as_mut()
                        .unwrap()
                        .pf_id = "02:00:00:00:00:00".to_string();
                },
                expect: FailsWith(
                    "missing matching DOCA weave attachment for MAC 02:aa:bb:cc:dd:ff VNI 100"
                        .to_string(),
                ),
            },
            Case {
                scenario: "extra ready network",
                input: |resources| {
                    resources
                        .networks
                        .push(weave_ew_vpc_virtual_network("astra-weave-vni-200", 200))
                },
                expect: FailsWith(
                    "virtual network count mismatch: expected 1, observed 2".to_string(),
                ),
            },
            Case {
                scenario: "wrong network ID",
                input: |resources| {
                    resources.networks[0].metadata.as_mut().unwrap().id =
                        Some("wrong-id".to_string())
                },
                expect: FailsWith(
                    "virtual network astra-weave-vni-100 has an unexpected ID or is deleting"
                        .to_string(),
                ),
            },
            Case {
                scenario: "duplicate network VNI with equal network count",
                input: |resources| {
                    // Keep two expected and two observed networks so the
                    // check for duplicate VNIs, rather than count, rejects this.
                    resources.desired.astra_attachments[1].vni = 200;
                    resources.attachments[1].spec.as_mut().unwrap().vnet_id =
                        "astra-weave-vni-200".to_string();
                    resources.networks.push(resources.networks[0].clone());
                },
                expect: FailsWith("duplicate virtual network VNI 100".to_string()),
            },
            Case {
                scenario: "unexpected VNI with equal network count",
                input: |resources| {
                    resources.networks[0] = weave_ew_vpc_virtual_network("astra-weave-vni-200", 200)
                },
                expect: FailsWith("unexpected DOCA weave virtual network VNI 200".to_string()),
            },
        ];
        check_cases(cases, |mutate| {
            let mut resources = Resources {
                desired: rpc::AstraConfig {
                    astra_attachments: vec![
                        astra_attachment("02:aa:bb:cc:dd:ee", 100),
                        astra_attachment("02:aa:bb:cc:dd:ff", 100),
                    ],
                    ..Default::default()
                },
                networks: vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
                attachments: vec![
                    weave_ew_vpc_virtual_network_attachment(
                        "first-attachment",
                        "02:aa:bb:cc:dd:ee",
                        "astra-weave-vni-100",
                    ),
                    weave_ew_vpc_virtual_network_attachment(
                        "second-attachment",
                        "02:aa:bb:cc:dd:ff",
                        "astra-weave-vni-100",
                    ),
                ],
            };
            mutate(&mut resources);
            let desired = build_astra_config_status(&resources.desired).unwrap();
            verify_weave_ew_vpc_resources_match(
                &desired,
                &resources.networks,
                &resources.attachments,
            )
            .map_err(|error| error.to_string())
        });
    }

    /// Undeletable records keep the report Pending while valid stale records
    /// still get removed; readiness recovers once the malformed records are gone.
    #[tokio::test]
    async fn malformed_stale_ids_preserve_pending_without_blocking_other_cleanup()
    -> eyre::Result<()> {
        let mut malformed_network = weave_ew_vpc_virtual_network("astra-weave-vni-200", 200);
        malformed_network.metadata = None;
        let mut malformed_attachment = weave_ew_vpc_virtual_network_attachment(
            "malformed",
            "02:aa:bb:cc:dd:ff",
            "astra-weave-vni-200",
        );
        malformed_attachment.metadata.as_mut().unwrap().id = Some(String::new());
        let (socket, calls, state, server) = start_recording_weave_ew_vpc_mock_server_with_state(
            vec![
                weave_ew_vpc_virtual_network("astra-weave-vni-100", 100),
                malformed_network,
                weave_ew_vpc_virtual_network("astra-weave-vni-300", 300),
            ],
            vec![
                weave_ew_vpc_virtual_network_attachment(
                    "retained",
                    "02:aa:bb:cc:dd:ee",
                    "astra-weave-vni-100",
                ),
                malformed_attachment,
                weave_ew_vpc_virtual_network_attachment(
                    "valid-stale",
                    "02:aa:bb:cc:dd:00",
                    "astra-weave-vni-300",
                ),
            ],
            WeaveEwVpcPhase::Ready,
        );
        let config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
            ..Default::default()
        };

        // A malformed ID cannot be deleted, but it must not stop cleanup of
        // valid resources listed after it. The final observation stays Pending.
        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                .await?;
        assert_eq!(status.astra_attachments_status.len(), 1);
        let status = status.astra_attachments_status[0].status.as_ref().unwrap();
        assert_eq!(status.phase, AstraPhase::PhasePending as i32);
        assert_eq!(status.reason, "DOCA Weave state has not converged");
        assert!(status.message.contains("virtual network count mismatch"));
        {
            let calls = calls.lock().await;
            assert_eq!(calls.delete_virtual_networks.len(), 1);
            assert_eq!(calls.delete_virtual_networks[0].id, "astra-weave-vni-300");
            assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
            assert_eq!(
                calls.delete_virtual_network_attachments[0].id,
                "valid-stale"
            );
        }
        {
            let mut state = state.lock().await;
            assert_eq!(state.virtual_networks.len(), 2);
            assert_eq!(state.virtual_network_attachments.len(), 2);
            // Simulate an external repair removing records that have no usable
            // Delete ID. The reconciler itself cannot remove those records.
            state
                .virtual_networks
                .retain(|network| network.metadata.is_some());
            state.virtual_network_attachments.retain(|attachment| {
                attachment.metadata.as_ref().unwrap().id.as_deref() != Some("")
            });
        }
        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                .await?;
        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(
            status.astra_attachments_status[0]
                .status
                .as_ref()
                .unwrap()
                .phase,
            AstraPhase::PhaseReady as i32
        );
        server.shutdown().await?;
        Ok(())
    }

    /// Mutations can finish before the final observation fails. The next
    /// iteration must recognize that work from Lists without repeating it.
    #[tokio::test]
    async fn post_reconcile_read_failure_recovers_on_next_iteration() -> eyre::Result<()> {
        #[derive(Clone, Copy, Debug)]
        enum FailedList {
            Networks,
            Attachments,
        }

        for (failed_list, expected_error) in [
            (
                FailedList::Networks,
                "injected virtual network list failure",
            ),
            (FailedList::Attachments, "injected attachment list failure"),
        ] {
            let (socket, calls, state, server) =
                start_recording_weave_ew_vpc_mock_server_with_state(
                    vec![],
                    vec![],
                    WeaveEwVpcPhase::Ready,
                );
            // Initial observation and reconciliation use three network Lists
            // and two attachment Lists; calls 4 and 3 are the fresh observation.
            match failed_list {
                FailedList::Networks => state.lock().await.fail_network_list_on_call = Some(4),
                FailedList::Attachments => {
                    state.lock().await.fail_attachment_list_on_call = Some(3)
                }
            }
            let config = rpc::AstraConfig {
                astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
                ..Default::default()
            };
            let error =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await
                    .unwrap_err();
            assert!(
                format!("{error:#}").contains(expected_error),
                "{failed_list:?}: {error}"
            );
            {
                let mut calls = calls.lock().await;
                assert_eq!(calls.create_virtual_networks.len(), 1, "{failed_list:?}");
                assert_eq!(
                    calls.create_virtual_network_attachments.len(),
                    1,
                    "{failed_list:?}"
                );
                assert_eq!(calls.list_virtual_networks, 4);
                assert_eq!(
                    calls.list_virtual_network_attachments,
                    match failed_list {
                        FailedList::Networks => 2,
                        FailedList::Attachments => 3,
                    }
                );
                *calls = RecordedWeaveEwVpcCalls::default();
            }
            let status =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await?;
            assert_eq!(status.astra_attachments_status.len(), 1);
            assert_eq!(
                status.astra_attachments_status[0]
                    .status
                    .as_ref()
                    .unwrap()
                    .phase,
                AstraPhase::PhaseReady as i32
            );
            let calls = calls.lock().await;
            assert_eq!(calls.list_virtual_networks, 1);
            assert_eq!(calls.list_virtual_network_attachments, 1);
            assert!(calls.create_virtual_networks.is_empty(), "{failed_list:?}");
            assert!(calls.update_virtual_networks.is_empty(), "{failed_list:?}");
            assert!(calls.delete_virtual_networks.is_empty(), "{failed_list:?}");
            assert!(
                calls.create_virtual_network_attachments.is_empty(),
                "{failed_list:?}"
            );
            assert!(
                calls.update_virtual_network_attachments.is_empty(),
                "{failed_list:?}"
            );
            assert!(
                calls.delete_virtual_network_attachments.is_empty(),
                "{failed_list:?}"
            );
            server.shutdown().await?;
        }
        Ok(())
    }

    /// List timeouts prevent observation and return Err; a Create timeout is
    /// retained as an attachment Error when no created resource can be observed.
    #[tokio::test]
    async fn reconciliation_maps_request_timeouts_to_errors_and_attachment_status() {
        #[derive(Clone, Copy, Debug)]
        enum StalledRequest {
            InitialNetworkList,
            InitialAttachmentList,
            ReconcileNetworkList,
            CreateNetwork,
        }

        for stalled_request in [
            StalledRequest::InitialNetworkList,
            StalledRequest::InitialAttachmentList,
            StalledRequest::ReconcileNetworkList,
            StalledRequest::CreateNetwork,
        ] {
            let (socket, _, state, server) = start_recording_weave_ew_vpc_mock_server_with_state(
                vec![],
                vec![],
                WeaveEwVpcPhase::Ready,
            );
            let entered = Arc::new(tokio::sync::Notify::new());
            let operation = {
                let mut state = state.lock().await;
                match stalled_request {
                    StalledRequest::InitialNetworkList | StalledRequest::ReconcileNetworkList => {
                        state.stall_network_list = Some(entered.clone());
                        state.stall_network_list_on_call = Some(match stalled_request {
                            StalledRequest::ReconcileNetworkList => 2,
                            _ => 1,
                        });
                        "ListVirtualNetworks"
                    }
                    StalledRequest::InitialAttachmentList => {
                        state.stall_attachment_list = Some(entered.clone());
                        "ListVirtualNetworkAttachments"
                    }
                    StalledRequest::CreateNetwork => {
                        state.stall_network_create = Some(entered.clone());
                        "CreateVirtualNetwork"
                    }
                }
            };
            let reconcile = tokio::spawn(async move {
                let config = rpc::AstraConfig {
                    astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
                    ..Default::default()
                };
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await
            });
            // Establish the stalled RPC with real I/O, then advance its budget.
            // Resume before the remaining RPCs: this test checks error mapping;
            // the client tests own the exact request-deadline boundary.
            tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            tokio::time::pause();
            tokio::time::advance(std::time::Duration::from_secs(10)).await;
            tokio::time::resume();
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), reconcile)
                .await
                .unwrap()
                .unwrap();
            let diagnostic = match stalled_request {
                StalledRequest::CreateNetwork => {
                    let status = result.unwrap();
                    assert_eq!(status.astra_attachments_status.len(), 1);
                    let status = status.astra_attachments_status[0].status.as_ref().unwrap();
                    assert_eq!(status.phase, AstraPhase::PhaseError as i32);
                    assert_eq!(status.reason, "API failure");
                    status.message.clone()
                }
                _ => result.unwrap_err().to_string(),
            };
            assert!(
                diagnostic.contains(&format!("{operation} timed out after 10s")),
                "{stalled_request:?}: {diagnostic}"
            );
            server.shutdown().await.unwrap();
        }
    }

    /// When Delete fails before changing service state, reconciliation must
    /// report the failure without creating a replacement, then retry later.
    #[tokio::test]
    async fn mismatched_attachment_delete_failure_preserves_binding_until_retry() -> eyre::Result<()>
    {
        #[derive(Clone, Copy, Debug)]
        enum Mismatch {
            Bridge,
            Vni,
        }

        for (mismatch, failure, expected_error) in [
            (
                Mismatch::Bridge,
                MockRpcFailure::Stall(Arc::new(tokio::sync::Notify::new())),
                "DeleteVirtualNetworkAttachment timed out after 10s",
            ),
            (
                Mismatch::Vni,
                MockRpcFailure::Unavailable,
                "injected attachment RPC failure",
            ),
        ] {
            let old_attachment = weave_ew_vpc_virtual_network_attachment_ovs(
                "old-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
                "br-old",
                "test-revision",
            );
            let mut networks = vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)];
            let (desired_vni, desired_bridge) = match mismatch {
                Mismatch::Bridge => (100, "br-new"),
                Mismatch::Vni => {
                    networks.push(weave_ew_vpc_virtual_network("astra-weave-vni-200", 200));
                    (200, "br-old")
                }
            };
            let (socket, calls, state, server) =
                start_recording_weave_ew_vpc_mock_server_with_state(
                    networks,
                    vec![old_attachment.clone()],
                    WeaveEwVpcPhase::Ready,
                );
            state.lock().await.attachment_delete_failure = Some(failure.clone());
            // A binding change uses the exact NIC/VNI branch; a VNI change uses
            // the conflicting-attachment branch and is also stale during cleanup.
            let config = rpc::AstraConfig {
                astra_attachments: vec![astra_attachment_ovs(
                    "02:aa:bb:cc:dd:ee",
                    desired_vni,
                    desired_bridge,
                    "test-revision",
                )],
                ..Default::default()
            };
            let result = reconcile_with_injected_rpc_failure(&socket, &config, &failure).await;
            let expected_delete_attempts = match mismatch {
                Mismatch::Bridge => {
                    let status = result?;
                    assert_eq!(status.astra_attachments_status.len(), 1);
                    let status = status.astra_attachments_status[0].status.as_ref().unwrap();
                    assert_eq!(status.phase, AstraPhase::PhaseError as i32);
                    assert_eq!(
                        status.reason,
                        "Failed to delete mismatched Doca Weave attachment"
                    );
                    assert!(status.message.contains(expected_error), "{status:?}");
                    1
                }
                Mismatch::Vni => {
                    // Stale cleanup retries the old VNI attachment after the
                    // replacement pass. Its failed Delete aborts reconciliation.
                    let error = format!("{:#}", result.unwrap_err());
                    assert!(
                        error.contains("failed to delete stale virtual network attachment"),
                        "{error}"
                    );
                    assert!(error.contains(expected_error), "{error}");
                    2
                }
            };
            {
                let mut calls = calls.lock().await;
                assert_eq!(
                    calls.delete_virtual_network_attachments.len(),
                    expected_delete_attempts,
                    "{mismatch:?}"
                );
                assert!(
                    calls
                        .delete_virtual_network_attachments
                        .iter()
                        .all(|request| request.id == "old-attachment"),
                    "{mismatch:?}"
                );
                assert!(
                    calls.create_virtual_network_attachments.is_empty(),
                    "{mismatch:?}"
                );
                assert!(
                    calls.update_virtual_network_attachments.is_empty(),
                    "{mismatch:?}"
                );
                assert!(calls.delete_virtual_networks.is_empty(), "{mismatch:?}");
                *calls = RecordedWeaveEwVpcCalls::default();
            }
            {
                let mut state = state.lock().await;
                assert_eq!(
                    state.virtual_network_attachments,
                    vec![old_attachment],
                    "{mismatch:?}"
                );
                // Remove only the injected failure. Reconciliation must do
                // the actual deletion and replacement on the next attempt.
                state.attachment_delete_failure = None;
            }
            let status =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await?;
            assert_eq!(status.astra_attachments_status.len(), 1, "{mismatch:?}");
            assert_eq!(
                status.astra_attachments_status[0]
                    .status
                    .as_ref()
                    .unwrap()
                    .phase,
                AstraPhase::PhaseReady as i32,
                "{mismatch:?}"
            );
            {
                let calls = calls.lock().await;
                assert_eq!(
                    calls.delete_virtual_network_attachments.len(),
                    1,
                    "{mismatch:?}"
                );
                assert_eq!(
                    calls.delete_virtual_network_attachments[0].id,
                    "old-attachment"
                );
                assert_eq!(
                    calls.create_virtual_network_attachments.len(),
                    1,
                    "{mismatch:?}"
                );
                assert!(
                    calls.update_virtual_network_attachments.is_empty(),
                    "{mismatch:?}"
                );
                match mismatch {
                    Mismatch::Bridge => assert!(calls.delete_virtual_networks.is_empty()),
                    Mismatch::Vni => {
                        assert_eq!(calls.delete_virtual_networks.len(), 1);
                        assert_eq!(calls.delete_virtual_networks[0].id, "astra-weave-vni-100");
                    }
                }
            }
            {
                let state = state.lock().await;
                assert_eq!(state.virtual_network_attachments.len(), 1, "{mismatch:?}");
                let spec = state.virtual_network_attachments[0].spec.as_ref().unwrap();
                assert_eq!(spec.nic_id, "02:aa:bb:cc:dd:ee");
                assert_eq!(spec.vnet_id, format!("astra-weave-vni-{desired_vni}"));
                assert_eq!(
                    spec.attachment_ovs.as_ref().unwrap().bridge_name,
                    desired_bridge
                );
            }
            server.shutdown().await?;
        }
        Ok(())
    }

    /// Create can commit even when its reply fails. A fresh complete observation
    /// must report that attachment Ready, and the next iteration must not recreate it.
    #[tokio::test]
    async fn committed_attachment_create_survives_response_failure_without_duplicate()
    -> eyre::Result<()> {
        for failure in [
            MockRpcFailure::Unavailable,
            MockRpcFailure::Stall(Arc::new(tokio::sync::Notify::new())),
        ] {
            let (socket, calls, state, server) =
                start_recording_weave_ew_vpc_mock_server_with_state(
                    vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
                    vec![],
                    WeaveEwVpcPhase::Ready,
                );
            state.lock().await.attachment_create_response_failure = Some(failure.clone());
            let config = rpc::AstraConfig {
                astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
                ..Default::default()
            };
            let status = reconcile_with_injected_rpc_failure(&socket, &config, &failure).await?;
            assert_eq!(status.astra_attachments_status.len(), 1, "{failure:?}");
            let attachment = &status.astra_attachments_status[0];
            assert_eq!(attachment.mac_address, "02:aa:bb:cc:dd:ee");
            assert_eq!(attachment.vni, 100);
            assert_eq!(attachment.revision, "test-revision");
            assert_eq!(
                attachment.status.as_ref().unwrap().phase,
                AstraPhase::PhaseReady as i32,
                "{failure:?}"
            );
            {
                let state = state.lock().await;
                assert_eq!(state.virtual_network_attachments.len(), 1, "{failure:?}");
                let spec = state.virtual_network_attachments[0].spec.as_ref().unwrap();
                assert_eq!(spec.nic_id, "02:aa:bb:cc:dd:ee");
                assert_eq!(spec.vnet_id, "astra-weave-vni-100");
            }
            {
                let mut calls = calls.lock().await;
                assert_eq!(
                    calls.create_virtual_network_attachments.len(),
                    1,
                    "{failure:?}"
                );
                assert!(
                    calls.update_virtual_network_attachments.is_empty(),
                    "{failure:?}"
                );
                assert!(
                    calls.delete_virtual_network_attachments.is_empty(),
                    "{failure:?}"
                );
                *calls = RecordedWeaveEwVpcCalls::default();
            }

            // Leave the response failure enabled: observing the stored
            // attachment must be enough, without issuing another Create at all.
            let next_status =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await?;
            assert_eq!(next_status, status, "{failure:?}");
            let calls = calls.lock().await;
            assert_eq!(calls.list_virtual_networks, 1, "{failure:?}");
            assert_eq!(calls.list_virtual_network_attachments, 1, "{failure:?}");
            assert!(calls.create_virtual_networks.is_empty(), "{failure:?}");
            assert!(calls.update_virtual_networks.is_empty(), "{failure:?}");
            assert!(calls.delete_virtual_networks.is_empty(), "{failure:?}");
            assert!(
                calls.create_virtual_network_attachments.is_empty(),
                "{failure:?}"
            );
            assert!(
                calls.update_virtual_network_attachments.is_empty(),
                "{failure:?}"
            );
            assert!(
                calls.delete_virtual_network_attachments.is_empty(),
                "{failure:?}"
            );
            server.shutdown().await?;
        }
        Ok(())
    }

    /// A desired binding that cannot be constructed must not destroy the usable
    /// attachment already present for the same NIC and VNI.
    #[tokio::test]
    async fn invalid_desired_binding_preserves_the_working_attachment() -> eyre::Result<()> {
        let working_attachment = weave_ew_vpc_virtual_network_attachment(
            "working",
            "02:aa:bb:cc:dd:ee",
            "astra-weave-vni-100",
        );
        let (socket, calls, state, server) = start_recording_weave_ew_vpc_mock_server_with_state(
            vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
            vec![working_attachment.clone()],
            WeaveEwVpcPhase::Ready,
        );
        let mut desired = astra_attachment("02:aa:bb:cc:dd:ee", 100);
        // Select VF attachment without the required VF details. This exercises
        // construction failure before the working PF attachment can be deleted.
        desired.attachment_type = Some(rpc::SpxAttachmentType::Virtual as i32);
        let config = rpc::AstraConfig {
            astra_attachments: vec![desired],
            ..Default::default()
        };
        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                .await?;
        assert_eq!(status.astra_attachments_status.len(), 1);
        let status = status.astra_attachments_status[0].status.as_ref().unwrap();
        assert_eq!(status.phase, AstraPhase::PhaseError as i32);
        assert_eq!(status.reason, "Missing Astra virtual_function_id");
        let calls = calls.lock().await;
        assert!(calls.delete_virtual_network_attachments.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert!(calls.update_virtual_network_attachments.is_empty());
        assert_eq!(
            state.lock().await.virtual_network_attachments,
            vec![working_attachment]
        );
        server.shutdown().await?;
        Ok(())
    }

    /// Empty desired configuration has no attachment to hold a Pending reason,
    /// so incomplete cleanup returns an error until the service inventory is empty.
    #[tokio::test]
    async fn empty_config_returns_error_until_cleanup_disappears() -> eyre::Result<()> {
        let (socket, calls, state, server) = start_recording_weave_ew_vpc_mock_server_with_state(
            vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
            vec![weave_ew_vpc_virtual_network_attachment(
                "stale",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
            WeaveEwVpcPhase::Ready,
        );
        // A successful Delete response alone cannot acknowledge empty desired
        // state while the service still lists the resources being removed.
        state.lock().await.retain_deleted_resources = true;
        let config = rpc::AstraConfig::default();
        let error =
            build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                .await
                .unwrap_err();
        assert!(error.to_string().contains("virtual network count mismatch"));
        {
            let calls = calls.lock().await;
            assert_eq!(calls.delete_virtual_networks.len(), 1);
            assert_eq!(calls.delete_virtual_networks[0].id, "astra-weave-vni-100");
            assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
            assert_eq!(calls.delete_virtual_network_attachments[0].id, "stale");
        }
        {
            let mut state = state.lock().await;
            // Complete the service's asynchronous deletion between iterations.
            state.virtual_networks.clear();
            state.virtual_network_attachments.clear();
        }
        let status =
            build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                .await?;
        assert!(status.astra_attachments_status.is_empty());
        server.shutdown().await?;
        Ok(())
    }

    /// Revision metadata updates cannot repair an immutable subnet mismatch.
    /// Repeated reconciliation must keep reporting Pending without deleting it.
    #[tokio::test]
    async fn subnet_drift_remains_pending_across_reconciliation_iterations() -> eyre::Result<()> {
        let mut network = weave_ew_vpc_virtual_network("astra-weave-vni-100", 100);
        // Leave identity and revision matching to isolate subnet drift.
        network.spec.as_mut().unwrap().subnet_ipv4 = Some("198.51.100.0/24".to_string());
        let (socket, calls, server) = start_recording_weave_ew_vpc_mock_server(
            vec![network],
            vec![weave_ew_vpc_virtual_network_attachment(
                "retained",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
        );
        let config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
            ..Default::default()
        };
        for iteration in 0..2 {
            let status =
                build_notify_weave_ew_vpc_astra_config_uds(socket.to_str().unwrap(), Some(&config))
                    .await?;
            assert_eq!(status.astra_attachments_status.len(), 1);
            let status = status.astra_attachments_status[0].status.as_ref().unwrap();
            assert_eq!(
                status.phase,
                AstraPhase::PhasePending as i32,
                "iteration {iteration}"
            );
            assert_eq!(status.reason, "DOCA Weave state has not converged");
            assert!(status.message.contains("subnet does not match"));
        }
        let calls = calls.lock().await;
        assert_eq!(calls.update_virtual_networks.len(), 2);
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.create_virtual_networks.is_empty());
        server.shutdown().await?;
        Ok(())
    }

    #[test]
    fn test_build_astra_config_status_copies_attachments_and_sets_ready() -> eyre::Result<()> {
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![
                astra_attachment("00:11:22:33:44:55", 100),
                astra_attachment("00:11:22:33:44:66", 200),
            ],
        };

        let status = build_astra_config_status(&astra_config)?;

        assert_eq!(status.astra_attachments_status.len(), 2);
        assert_eq!(status.is_attachment_status_present, Some(true));
        assert_eq!(status.is_maintenance_status_present, Some(false));

        let first_status = &status.astra_attachments_status[0];
        assert_eq!(first_status.mac_address, "00:11:22:33:44:55");
        assert_eq!(first_status.vni, 100);
        assert_eq!(first_status.subnet_ipv4, "192.0.2.0");
        assert_eq!(first_status.subnet_mask, 24);
        assert_eq!(
            first_status.attachment_type,
            Some(rpc::SpxAttachmentType::Physical as i32)
        );
        assert_eq!(first_status.attachment_vf, None);
        assert_eq!(first_status.attachment_ovs, None);
        assert_eq!(first_status.revision, "test-revision");

        let first_phase = first_status.status.as_ref().map(|status| status.phase);
        assert_eq!(first_phase, Some(rpc::AstraPhase::PhaseReady as i32));

        Ok(())
    }

    #[test]
    fn test_build_astra_config_status_rejects_vni_that_does_not_fit_i32() {
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![astra_attachment("00:11:22:33:44:55", i32::MAX as u32 + 1)],
        };

        let err = build_astra_config_status(&astra_config).unwrap_err();

        assert!(
            err.to_string().contains("does not fit in i32"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn test_build_astra_config_status_rejects_missing_revision() {
        let mut attachment = astra_attachment("00:11:22:33:44:55", 100);
        attachment.revision.clear();
        let astra_config = rpc::AstraConfig {
            multiplane_interfaces: Vec::new(),
            is_attachment_config_present: None,
            is_maintenance_config_present: None,
            astra_attachments: vec![attachment],
        };

        let err = build_astra_config_status(&astra_config).unwrap_err();

        assert!(
            err.to_string().contains("missing revision"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn test_update_astra_attachment_status_maps_vpc_phase_to_astra_phase() {
        let cases = [
            (WeaveEwVpcPhase::Ready, rpc::AstraPhase::PhaseReady),
            (WeaveEwVpcPhase::Error, rpc::AstraPhase::PhaseError),
            (WeaveEwVpcPhase::Pending, rpc::AstraPhase::PhasePending),
            (WeaveEwVpcPhase::Deleting, rpc::AstraPhase::PhaseDeleting),
            (
                WeaveEwVpcPhase::Unspecified,
                rpc::AstraPhase::PhaseUnspecified,
            ),
        ];

        for (vpc_phase, expected_astra_phase) in cases {
            let mut attachment_status = build_astra_config_status(&rpc::AstraConfig {
                multiplane_interfaces: Vec::new(),
                is_attachment_config_present: None,
                is_maintenance_config_present: None,
                astra_attachments: vec![astra_attachment("00:11:22:33:44:55", 100)],
            })
            .unwrap()
            .astra_attachments_status
            .remove(0);

            set_astra_attachment_status_with_weave_ew_vpc_status(
                &mut attachment_status,
                State {
                    phase: vpc_phase as i32,
                    reason: "weave-ew-vpc-reason".to_string(),
                    message: "weave-ew-vpc-message".to_string(),
                },
            );

            let status = attachment_status.status.unwrap();
            assert_eq!(status.phase, expected_astra_phase as i32);
            assert_eq!(status.reason, "weave-ew-vpc-reason");
            assert_eq!(status.message, "weave-ew-vpc-message");
        }
    }
}
