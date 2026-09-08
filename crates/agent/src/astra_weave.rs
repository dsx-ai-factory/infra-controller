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
    AttachmentOvn, AttachmentPf, AttachmentType, AttachmentVf,
    CreateVirtualNetworkAttachmentRequest, CreateVirtualNetworkRequest,
    DeleteVirtualNetworkAttachmentRequest, DeleteVirtualNetworkRequest,
    ListVirtualNetworkAttachmentsRequest, ListVirtualNetworksRequest, ObjectMetadata, State,
    VirtualNetworkAttachment, VirtualNetworkAttachmentSpec, VirtualNetworkSpec,
};
use crate::weave_ew_vpc_client::{
    WEAVE_EW_VPC_FLOW_CONTROLLER_SOCKET_PATH, weave_ew_vpc_create_virtual_network,
    weave_ew_vpc_create_virtual_network_attachment, weave_ew_vpc_delete_virtual_network,
    weave_ew_vpc_delete_virtual_network_attachment, weave_ew_vpc_list_virtual_network_attachments,
    weave_ew_vpc_list_virtual_networks,
};

fn astra_weave_ew_vpc_virtual_network_id(vni: i32) -> String {
    format!("astra-weave-vni-{vni}")
}

const WEAVE_EW_VPC_REVISION_USER_DATA_KEY: &str = "revision";

fn weave_ew_vpc_object_metadata(id: Option<String>, revision: &str) -> ObjectMetadata {
    ObjectMetadata {
        id,
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
        attachment_ovn: None,
    };

    match astra_attachment_type {
        SpxAttachmentType::Physical => {
            spec.attachment_type = AttachmentType::Pf.into();
            spec.attachment_pf = Some(AttachmentPf {
                pf_id: astra_attachment_status.mac_address.clone(),
            });
        }
        SpxAttachmentType::Virtual => {
            let Some(virtual_function_id) = astra_attachment_status.virtual_function_id else {
                return Err(State {
                    phase: Phase::Error.into(),
                    reason: "Missing Astra virtual_function_id".to_string(),
                    message: "create_virtual_network_attachment".to_string(),
                });
            };
            let Ok(vf_index) = u32::try_from(virtual_function_id) else {
                return Err(State {
                    phase: Phase::Error.into(),
                    reason: "Invalid Astra virtual_function_id".to_string(),
                    message: "create_virtual_network_attachment".to_string(),
                });
            };

            spec.attachment_type = AttachmentType::Vf.into();
            spec.attachment_vf = Some(AttachmentVf {
                pf_id: astra_attachment_status.mac_address.clone(),
                vf_index,
            });
        }
        SpxAttachmentType::Ovn => {
            let Some(network_name) = astra_attachment_status
                .network_name
                .as_ref()
                .filter(|network_name| !network_name.is_empty())
            else {
                return Err(State {
                    phase: Phase::Error.into(),
                    reason: "Missing Astra OVN network_name".to_string(),
                    message: "create_virtual_network_attachment".to_string(),
                });
            };

            spec.attachment_type = AttachmentType::Ovn.into();
            spec.attachment_ovn = Some(AttachmentOvn {
                network_name: network_name.clone(),
            });
        }
    }

    Ok(spec)
}

// Take a diff of the Astra config vs the DOCA Weave server virtual networks
// and create new virtual networks that are missing on the server.
async fn create_weave_ew_vpc_virtual_networks(
    socket_path: &str,
    astra_config_status: &mut AstraConfigStatus,
) -> eyre::Result<()> {
    // Get list of existing virtual networks from the Doca Weave server
    let list_vni_req = ListVirtualNetworksRequest { vni: None };
    let list_vni_rsp = weave_ew_vpc_list_virtual_networks(socket_path, list_vni_req).await?;

    // Log virtual networks on the weave server.
    log_virtual_networks(&list_vni_rsp.virtual_networks);

    // From the list of virtual networks on the DOCA Weave server, build a
    // seen_virtual_networks HashMap of (vni, (id, state)) for each virtual
    // network. If weave does not return a status for a virtual network,
    // we consider it unusable. Note that seen_virtual_networks may be empty.
    let mut seen_virtual_networks: HashMap<u32, (Option<String>, State)> = list_vni_rsp
        .virtual_networks
        .iter()
        .filter_map(|virtual_network| {
            let vni = virtual_network.spec.as_ref()?.vni;
            let id = virtual_network
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.id.clone())
                .none_if_empty();
            let state = virtual_network
                .status
                .as_ref()
                .and_then(|status| status.state.clone())
                .unwrap_or_else(|| State {
                    phase: Phase::Error.into(),
                    reason: "Response is missing state".to_string(),
                    message: "list_virtual_networks".to_string(),
                });
            Some((vni, (id, state)))
        })
        .collect();

    // For each attachment in astra status, compare {vni, virtual-network-id}
    // against the seen_virtual_network.
    for astra_attachment_status in &mut astra_config_status.astra_attachments_status {
        // A vni of 0 indicates an admin-network. The Weave server rejects
        // vni 0, so don't create a virtual network for it.
        if astra_attachment_status.vni == 0 {
            continue;
        }

        // If exact match then this virtual network exists on weave. If
        // status is in error, copy that status to the astra_attachment_status.
        // If partial match, then also mark as error in astra_attachment_status.
        let astra_vni = astra_attachment_status.vni as u32;
        let astra_virtual_network_id =
            astra_weave_ew_vpc_virtual_network_id(astra_attachment_status.vni);
        if let Some((virtual_network_id, weave_ew_vpc_state)) =
            seen_virtual_networks.get(&astra_vni)
        {
            if virtual_network_id.as_deref() != Some(astra_virtual_network_id.as_str()) {
                set_astra_attachment_status_with_weave_ew_vpc_status(
                    astra_attachment_status,
                    State {
                        phase: Phase::Error.into(),
                        reason: "Conflicting DOCA Weave virtual network ID".to_string(),
                        message: format!(
                            "VNI {astra_vni} exists as {:?}, expected {astra_virtual_network_id}",
                            virtual_network_id
                        ),
                    },
                );
                continue;
            }

            if weave_ew_vpc_state.phase != Phase::Ready as i32 {
                set_astra_attachment_status_with_weave_ew_vpc_status(
                    astra_attachment_status,
                    weave_ew_vpc_state.clone(),
                );
            }
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
                    seen_virtual_networks.insert(
                        astra_vni,
                        (
                            Some(astra_virtual_network_id.clone()),
                            weave_ew_vpc_state.clone(),
                        ),
                    );
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
                "Cannot delete virtual network from DOCA Weave server because metadata id is missing or empty"
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
                    "Deleted stale virtual network from DOCA Weave server"
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

// Take a diff of Doca Weave Server vs Astra Attachments (both ways)
// and create or delete attachments as needed. Handle special case
// where an attachment may have changed its partition aka vni. This
// case is handled by deleting the existing attachment and recreating
// a new one with the new VNI.
async fn update_weave_ew_vpc_astra_attachments(
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
        // Skip any attachments where the status is not ready
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
                        spec.nic_id.as_str() == astra_attachment_status.mac_address.as_str()
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

        // Skip create for exact matching attachments.
        if let Some(exact_attachment) = exact_attachment {
            let weave_ew_vpc_state = exact_attachment
                .status
                .as_ref()
                .and_then(|status| status.state.clone())
                .unwrap_or_else(|| State {
                    phase: Phase::Error.into(),
                    reason: "Missing Doca Weave Server Status State".to_string(),
                    message: "list_virtual_network_attachments".to_string(),
                });
            set_astra_attachment_status_with_weave_ew_vpc_status(
                astra_attachment_status,
                weave_ew_vpc_state,
            );
            continue;
        }

        // create new or recreate mismatched vni attachments.
        create_or_recreate_weave_ew_vpc_astra_attachment(socket_path, astra_attachment_status)
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

async fn create_or_recreate_weave_ew_vpc_astra_attachment(
    socket_path: &str,
    astra_attachment_status: &mut AstraAttachmentStatus,
) -> eyre::Result<()> {
    // Ignore attachments in error state
    let virtual_network_attachment_spec =
        match weave_ew_virtual_network_attachment_spec_from_astra_attachment(
            astra_attachment_status,
        ) {
            Ok(virtual_network_attachment_spec) => virtual_network_attachment_spec,
            Err(weave_ew_vpc_state) => {
                set_astra_attachment_status_with_weave_ew_vpc_status(
                    &mut *astra_attachment_status,
                    weave_ew_vpc_state,
                );
                return Ok(());
            }
        };

    let weave_ew_vpc_attachment_create_req = CreateVirtualNetworkAttachmentRequest {
        metadata: Some(weave_ew_vpc_object_metadata(
            None,
            &astra_attachment_status.revision,
        )),
        spec: Some(virtual_network_attachment_spec),
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
                "Cannot delete virtual network attachment from DOCA Weave server because metadata id is missing or empty"
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
                    "Deleted stale virtual network attachment from DOCA Weave server"
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
                    spec.nic_id == astra_attachment_status.mac_address.as_str()
                        && spec.vnet_id
                            == astra_weave_ew_vpc_virtual_network_id(astra_attachment_status.vni)
                })
        })
}

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
            "Cannot delete mismatched virtual network attachment from DOCA Weave server as metadata id missing or empty"
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
                "Deleted mismatched virtual network attachment from DOCA Weave server"
            );
        }
        Err(err) => {
            tracing::error!(
                error = format!("{err:#}"),
                ?match_attachment,
                "Failed to delete mismatched virtual network attachment from DOCA Weave server"
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

// This is the main entry point into this module. The agent main_loop calls
// this function during every iteration with the AstraConfig supplied by
// Carbide.
pub(super) async fn update_weave_ew_vpc_astra_config(
    astra_config: Option<&AstraConfig>,
) -> eyre::Result<AstraConfigStatus> {
    update_weave_ew_vpc_astra_config_uds(WEAVE_EW_VPC_FLOW_CONTROLLER_SOCKET_PATH, astra_config)
        .await
}

// This is the internal function that is called by the main loop handler
// and tests (with a socketpath).
async fn update_weave_ew_vpc_astra_config_uds(
    socket_path: &str,
    astra_config: Option<&AstraConfig>,
) -> eyre::Result<AstraConfigStatus> {
    let Some(astra_config) = astra_config else {
        return Ok(AstraConfigStatus {
            astra_attachments_status: Vec::new(),
        });
    };

    debug_check_astra_config_attachment_revisions(astra_config);

    // There is a revision string associated with the AstraConfig that
    // is used to track changes to the AstraConfig. The configs don't change
    // if the revision string is unchanged. So at the onset of the routine
    // we see if the revision string has changed, and only if it has we do
    // further processing of the AstraConfig. One nit is that if the version
    // string is unchanged, we still have to copy the latest state from
    // the Doca Weave server into the astra_config_status as this could have
    // changed (for example, from Pending to Ready) while the main loop
    // was cycling.
    if let Some(astra_config_status) =
        build_synced_astra_config_status_if_version_unchanged(socket_path, astra_config).await?
    {
        return Ok(astra_config_status);
    }

    // At this point we know the AstraConfig has changed. Log the new
    // astra config.
    log_astra_config(astra_config);

    // Pre-build astra_config_status as a vector of AstraAttachmentStatus
    // that contains the Astra Attachment info and status is set to
    // Phase::Ready. We will walk this vector and update the status
    // as we update the DOCA Weave server and if there are any
    // API failures. We use this vector to avoid unneeded walking of
    // an entry that has encountered an error.
    let mut astra_config_status = build_astra_config_status(astra_config)?;

    // Update Doca Weave server with new astra config in order below.
    // 1. Create missing virtual networks on the DOCA Weave server.
    // 2. Create missing virtual network attachments, keep exact matches,
    // delete stale attachments, and recreate attachments whose VNI
    // (partition) has changed on the server.
    // 3. Delete stale virtual networks on the server.
    create_weave_ew_vpc_virtual_networks(socket_path, &mut astra_config_status).await?;

    update_weave_ew_vpc_astra_attachments(socket_path, &mut astra_config_status).await?;

    delete_stale_weave_ew_vpc_virtual_networks(socket_path, &astra_config_status).await?;

    Ok(astra_config_status)
}

async fn build_synced_astra_config_status_if_version_unchanged(
    socket_path: &str,
    astra_config: &AstraConfig,
) -> eyre::Result<Option<AstraConfigStatus>> {
    // Get the current spx_version associated with the AstraConfig.
    // You could use any attachment to get this, use the first one for
    // convenience. An empty AstraConfig has no revision to compare against, so
    // fall through to the full reconcile path (which deletes stale Weave state).
    let Some(first_attachment) = astra_config.astra_attachments.first() else {
        return Ok(None);
    };
    let nico_spx_version = astra_attachment_revision(first_attachment)?;

    // Get the spx_version for attachments installed on the DOCA Weave server.
    // We get the list of VNI attachments. In this list we could use any
    // attachment to get the revision string, use the first one for convenience.
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

    let weave_spx_version = list_vni_attachments_rsp
        .virtual_network_attachments
        .first()
        .and_then(|virtual_network_attachment| virtual_network_attachment.metadata.as_ref())
        .and_then(|metadata| metadata.user_data.get(WEAVE_EW_VPC_REVISION_USER_DATA_KEY))
        .map(String::as_str);

    // If version is changed, return so that we can update the Doca Weave
    // server with the latest astra config.
    if weave_spx_version != Some(nico_spx_version) {
        tracing::info!(
            weave_spx_version = weave_spx_version.unwrap_or("none"),
            nico_spx_version,
            "AstraConfig version changed; running full reconcile"
        );
        return Ok(None);
    }

    // Version matches, sync the status from Doca Weave to Astra Config
    // after locating a matching entry. If the server state has drifted from
    // the astra config despite the matching revision, fall back to a full
    // reconcile (return None) rather than erroring, so the drift is repaired.
    tracing::trace!(
        spx_version = %nico_spx_version,
        "AstraConfig version has no changes, copying attachment status"
    );
    match sync_astra_config_status_from_weave_ew_vpc_attachments(
        astra_config,
        &list_vni_attachments_rsp.virtual_network_attachments,
    ) {
        Ok(astra_config_status) => Ok(Some(astra_config_status)),
        Err(err) => {
            tracing::warn!(
                error = format!("{err:#}"),
                spx_version = %nico_spx_version,
                "AstraConfig version matches, but DOCA Weave state drifted; running full reconcile"
            );
            Ok(None)
        }
    }
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
            virtual_function_id: astra_attachment.virtual_function_id,
            network_name: astra_attachment.network_name.clone(),
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
            "AstraConfig attachment has missing revision"
        );
    }

    let first_revision = first_attachment.revision.as_str();
    for attachment in attachments.iter().skip(1) {
        if attachment.revision.is_empty() {
            tracing::error!(
                mac_address = %attachment.mac_address,
                "AstraConfig attachment has missing revision"
            );
            continue;
        }

        if attachment.revision != first_revision {
            tracing::error!(
                first_mac_address = %first_attachment.mac_address,
                first_revision,
                mac_address = %attachment.mac_address,
                revision = %attachment.revision,
                "AstraConfig attachment revisions are inconsistent"
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
                "DOCA Weave virtual network attachment revisions are inconsistent"
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
}

fn sync_astra_config_status_from_weave_ew_vpc_attachments(
    astra_config: &AstraConfig,
    virtual_network_attachments: &[VirtualNetworkAttachment],
) -> eyre::Result<AstraConfigStatus> {
    // Only attachments with a real vni (>= 1) have a corresponding attachment
    // on the Weave server. Detached NICs (vni 0) are attached to no virtual
    // network, so they are excluded from the count comparison.
    let expected_astra_attachment_count = astra_config
        .astra_attachments
        .iter()
        .filter(|astra_attachment| astra_attachment.vni != 0)
        .count();

    if expected_astra_attachment_count != virtual_network_attachments.len() {
        return Err(eyre::eyre!(
            "attachment count mismatch: AstraConfig has {expected_astra_attachment_count} attached NICs, DOCA weave server has {}",
            virtual_network_attachments.len()
        ));
    }

    let mut matched_weave_attachment_indices = HashSet::new();
    let mut astra_attachments_status = Vec::new();

    // Walk astra attachment to find a matching attachment in Doca Weave,
    // record any matched weave index to enforce a one-to-one match between
    // Astra and Weave attachments (aka the same entry is not used over for
    // matching). Its an error not if we don't find a matching attachment or
    // if the attachment does not have a status on the server.
    for astra_attachment in &astra_config.astra_attachments {
        // A detached NIC (vni 0) has no attachment on the Weave server; report
        // it Ready without requiring a matching server attachment.
        if astra_attachment.vni == 0 {
            let revision = astra_attachment_revision(astra_attachment)?;
            astra_attachments_status.push(AstraAttachmentStatus {
                mac_address: astra_attachment.mac_address.clone(),
                vni: 0,
                subnet_ipv4: astra_attachment.subnet_ipv4.clone(),
                subnet_mask: astra_attachment.subnet_mask,
                attachment_type: astra_attachment.attachment_type,
                virtual_function_id: astra_attachment.virtual_function_id,
                network_name: astra_attachment.network_name.clone(),
                revision: revision.to_string(),
                status: Some(AstraStatus {
                    phase: AstraPhase::PhaseReady.into(),
                    reason: String::new(),
                    message: String::new(),
                }),
            });
            continue;
        }

        let weave_attachment_match = virtual_network_attachments.iter().enumerate().find(
            |(index, virtual_network_attachment)| {
                !matched_weave_attachment_indices.contains(index)
                    && virtual_network_attachment
                        .spec
                        .as_ref()
                        .is_some_and(|spec| {
                            spec.nic_id == astra_attachment.mac_address.as_str()
                                && spec.vnet_id
                                    == astra_weave_ew_vpc_virtual_network_id(
                                        astra_attachment.vni as i32,
                                    )
                        })
            },
        );

        let Some((index, virtual_network_attachment)) = weave_attachment_match else {
            return Err(eyre::eyre!(
                "missing DOCA weave virtual network attachment for astra attachment mac_address={} vni={}",
                astra_attachment.mac_address,
                astra_attachment.vni
            ));
        };

        matched_weave_attachment_indices.insert(index);

        let Some(weave_ew_vpc_state) = virtual_network_attachment
            .status
            .as_ref()
            .and_then(|status| status.state.clone())
        else {
            return Err(eyre::eyre!(
                "DOCA weave virtual network attachment {:?} is missing status",
                virtual_network_attachment.spec
            ));
        };

        // Build the astra attachment status from the astra attachment
        // and copy the status from the Doca Weave Server
        let revision = astra_attachment_revision(astra_attachment)?;
        let mut astra_attachment_status = AstraAttachmentStatus {
            mac_address: astra_attachment.mac_address.clone(),
            vni: i32::try_from(astra_attachment.vni)
                .wrap_err_with(|| format!("VNI {} does not fit in i32", astra_attachment.vni))?,
            subnet_ipv4: astra_attachment.subnet_ipv4.clone(),
            subnet_mask: astra_attachment.subnet_mask,
            attachment_type: astra_attachment.attachment_type,
            virtual_function_id: astra_attachment.virtual_function_id,
            network_name: astra_attachment.network_name.clone(),
            revision: revision.to_string(),
            status: None,
        };
        set_astra_attachment_status_with_weave_ew_vpc_status(
            &mut astra_attachment_status,
            weave_ew_vpc_state,
        );
        astra_attachments_status.push(astra_attachment_status);
    }

    if matched_weave_attachment_indices.len() != virtual_network_attachments.len() {
        return Err(eyre::eyre!(
            "DOCA weave server has virtual network attachments not present in AstraConfig"
        ));
    }

    Ok(AstraConfigStatus {
        astra_attachments_status,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
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
        delete_virtual_networks: Vec<proto::DeleteVirtualNetworkRequest>,
        list_virtual_network_attachments: usize,
        create_virtual_network_attachments: Vec<proto::CreateVirtualNetworkAttachmentRequest>,
        delete_virtual_network_attachments: Vec<proto::DeleteVirtualNetworkAttachmentRequest>,
    }

    struct RecordedWeaveEwVpcState {
        virtual_networks: Vec<proto::VirtualNetwork>,
        virtual_network_attachments: Vec<proto::VirtualNetworkAttachment>,
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
            self.state
                .lock()
                .await
                .virtual_networks
                .retain(|virtual_network| {
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

        async fn list_virtual_networks(
            &self,
            _request: Request<proto::ListVirtualNetworksRequest>,
        ) -> Result<Response<proto::ListVirtualNetworksResponse>, Status> {
            self.calls.lock().await.list_virtual_networks += 1;

            Ok(Response::new(proto::ListVirtualNetworksResponse {
                virtual_networks: self.state.lock().await.virtual_networks.clone(),
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
            self.state
                .lock()
                .await
                .virtual_network_attachments
                .push(virtual_network_attachment.clone());

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
            self.state.lock().await.virtual_network_attachments.retain(
                |virtual_network_attachment| {
                    virtual_network_attachment
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.id.as_deref())
                        != Some(request.id.as_str())
                },
            );

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

        async fn list_virtual_network_attachments(
            &self,
            _request: Request<proto::ListVirtualNetworkAttachmentsRequest>,
        ) -> Result<Response<proto::ListVirtualNetworkAttachmentsResponse>, Status> {
            self.calls.lock().await.list_virtual_network_attachments += 1;

            Ok(Response::new(
                proto::ListVirtualNetworkAttachmentsResponse {
                    virtual_network_attachments: self
                        .state
                        .lock()
                        .await
                        .virtual_network_attachments
                        .clone(),
                },
            ))
        }
    }

    async fn start_recording_weave_ew_vpc_mock_server(
        virtual_networks: Vec<proto::VirtualNetwork>,
        virtual_network_attachments: Vec<proto::VirtualNetworkAttachment>,
    ) -> (PathBuf, Arc<Mutex<RecordedWeaveEwVpcCalls>>) {
        start_recording_weave_ew_vpc_mock_server_with_create_phase(
            virtual_networks,
            virtual_network_attachments,
            WeaveEwVpcPhase::Ready,
        )
        .await
    }

    async fn start_recording_weave_ew_vpc_mock_server_with_create_phase(
        virtual_networks: Vec<proto::VirtualNetwork>,
        virtual_network_attachments: Vec<proto::VirtualNetworkAttachment>,
        create_virtual_network_phase: WeaveEwVpcPhase,
    ) -> (PathBuf, Arc<Mutex<RecordedWeaveEwVpcCalls>>) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let _keep = dir.keep();
        let calls = Arc::new(Mutex::new(RecordedWeaveEwVpcCalls::default()));
        let state = Arc::new(Mutex::new(RecordedWeaveEwVpcState {
            virtual_networks,
            virtual_network_attachments,
        }));
        let service = RecordingNetworkIsolationService {
            state,
            calls: calls.clone(),
            create_virtual_network_phase,
        };
        let path_clone = socket_path.clone();
        tokio::spawn(async move {
            let listener = UnixListener::bind(path_clone).unwrap();
            let stream = UnixListenerStream::new(listener);
            tonic::transport::Server::builder()
                .add_service(NetworkIsolationServiceServer::new(service))
                .serve_with_incoming(stream)
                .await
                .unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (socket_path, calls)
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
            virtual_function_id: Some(7),
            network_name: Some("test-network".to_string()),
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
            virtual_function_id: None,
            network_name: None,
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
        proto::VirtualNetwork {
            metadata: Some(weave_ew_vpc_object_metadata(Some(id.to_string()), revision)),
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

    fn weave_ew_vpc_virtual_network_attachment_with_revision(
        id: &str,
        nic_id: &str,
        vnet_id: &str,
        revision: &str,
    ) -> proto::VirtualNetworkAttachment {
        proto::VirtualNetworkAttachment {
            metadata: Some(weave_ew_vpc_object_metadata(Some(id.to_string()), revision)),
            spec: Some(proto::VirtualNetworkAttachmentSpec {
                vnet_id: vnet_id.to_string(),
                nic_id: nic_id.to_string(),
                attachment_type: proto::AttachmentType::Pf.into(),
                attachment_pf: None,
                attachment_vf: None,
                attachment_ovn: None,
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
        let status = update_weave_ew_vpc_astra_config(None).await?;

        assert!(status.astra_attachments_status.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_adds_missing_vni_and_attachment()
    -> eyre::Result<()> {
        let (socket_path, calls) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new()).await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(calls.list_virtual_networks, 2);
        assert_eq!(calls.list_virtual_network_attachments, 2);
        assert_eq!(calls.create_virtual_networks.len(), 1);
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_fast_path_when_revision_matches()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
            vec![weave_ew_vpc_virtual_network_attachment(
                "matching-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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
        assert_eq!(calls.list_virtual_networks, 0);
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.delete_virtual_network_attachments.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_fast_path_with_detached_nic() -> eyre::Result<()>
    {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("astra-weave-vni-100", 100)],
            vec![weave_ew_vpc_virtual_network_attachment(
                "matching-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment_detached("02:aa:bb:cc:dd:ff", "test-revision"),
            ],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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
        assert_eq!(calls.list_virtual_networks, 0);
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.delete_virtual_network_attachments.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_revision_bump_does_not_recreate_matching_topology()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
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
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert!(calls.delete_virtual_networks.is_empty());
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.delete_virtual_network_attachments.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_astra_config_sync_drift_falls_back_to_full_reconcile()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
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
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
        assert_eq!(calls.list_virtual_network_attachments, 2);
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "stale-extra-attachment"
        );
        assert!(calls.create_virtual_network_attachments.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_creates_shared_vni_once()
    -> eyre::Result<()> {
        let (socket_path, calls) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new()).await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment("02:aa:bb:cc:dd:ff", 100),
            ],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_marks_pending_vni_seen()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server_with_create_phase(
            Vec::new(),
            Vec::new(),
            WeaveEwVpcPhase::Pending,
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment("02:aa:bb:cc:dd:ff", 100),
            ],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_marks_existing_pending_vni_not_ready()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network_with_phase(
                "astra-weave-vni-100",
                100,
                WeaveEwVpcPhase::Pending,
                "test-revision",
            )],
            Vec::new(),
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![
                astra_attachment("02:aa:bb:cc:dd:ee", 100),
                astra_attachment("02:aa:bb:cc:dd:ff", 100),
            ],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_deletes_stale_vni_and_attachment()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("stale-vni", 300)],
            vec![weave_ew_vpc_virtual_network_attachment(
                "stale-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-300",
            )],
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: Vec::new(),
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert!(status.astra_attachments_status.is_empty());
        assert_eq!(calls.list_virtual_networks, 2);
        assert_eq!(calls.list_virtual_network_attachments, 1);
        assert!(calls.create_virtual_networks.is_empty());
        assert!(calls.create_virtual_network_attachments.is_empty());
        assert_eq!(calls.delete_virtual_networks.len(), 1);
        assert_eq!(calls.delete_virtual_networks[0].id, "stale-vni");
        assert_eq!(calls.delete_virtual_network_attachments.len(), 1);
        assert_eq!(
            calls.delete_virtual_network_attachments[0].id,
            "stale-attachment"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_moves_attachment_to_new_vni()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
            vec![
                weave_ew_vpc_virtual_network("old-vni", 100),
                weave_ew_vpc_virtual_network("astra-weave-vni-200", 200),
            ],
            vec![weave_ew_vpc_virtual_network_attachment(
                "old-attachment",
                "02:aa:bb:cc:dd:ee",
                "astra-weave-vni-100",
            )],
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 200)],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_errors_on_conflicting_virtual_network_id()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
            vec![weave_ew_vpc_virtual_network("matching-vni", 100)],
            Vec::new(),
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment("02:aa:bb:cc:dd:ee", 100)],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_detached_nic_creates_nothing()
    -> eyre::Result<()> {
        let (socket_path, calls) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new()).await;
        let socket_path = socket_path.to_str().unwrap();
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment_detached("02:aa:bb:cc:dd:ee", "revision-1")],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_detaches_existing_attachment()
    -> eyre::Result<()> {
        let (socket_path, calls) = start_recording_weave_ew_vpc_mock_server(
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
        )
        .await;
        let socket_path = socket_path.to_str().unwrap();
        // The NIC previously attached to vni 100 is now detached (vni 0). The
        // revision bump forces a full reconcile.
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![astra_attachment_detached("02:aa:bb:cc:dd:ee", "revision-2")],
        };

        let status = update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await?;
        let calls = calls.lock().await;

        assert_eq!(status.astra_attachments_status.len(), 1);
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

        Ok(())
    }

    #[tokio::test]
    async fn test_update_weave_ew_vpc_server_astra_config_processes_full_config_sequence()
    -> eyre::Result<()> {
        let (socket_path, calls) =
            start_recording_weave_ew_vpc_mock_server(Vec::new(), Vec::new()).await;
        let socket_path = socket_path.to_str().unwrap();

        let config = |attachments: Vec<(&str, u32)>, revision: &str| rpc::AstraConfig {
            astra_attachments: attachments
                .into_iter()
                .map(|(mac_address, vni)| {
                    astra_attachment_with_revision(mac_address, vni, revision)
                })
                .collect(),
        };

        let run_update = |astra_config: rpc::AstraConfig| async move {
            update_weave_ew_vpc_astra_config_uds(socket_path, Some(&astra_config)).await
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

        Ok(())
    }

    #[test]
    fn test_build_astra_config_status_copies_attachments_and_sets_ready() -> eyre::Result<()> {
        let astra_config = rpc::AstraConfig {
            astra_attachments: vec![
                astra_attachment("00:11:22:33:44:55", 100),
                astra_attachment("00:11:22:33:44:66", 200),
            ],
        };

        let status = build_astra_config_status(&astra_config)?;

        assert_eq!(status.astra_attachments_status.len(), 2);

        let first_status = &status.astra_attachments_status[0];
        assert_eq!(first_status.mac_address, "00:11:22:33:44:55");
        assert_eq!(first_status.vni, 100);
        assert_eq!(first_status.subnet_ipv4, "192.0.2.0");
        assert_eq!(first_status.subnet_mask, 24);
        assert_eq!(
            first_status.attachment_type,
            Some(rpc::SpxAttachmentType::Physical as i32)
        );
        assert_eq!(first_status.virtual_function_id, Some(7));
        assert_eq!(first_status.network_name.as_deref(), Some("test-network"));
        assert_eq!(first_status.revision, "test-revision");

        let first_phase = first_status.status.as_ref().map(|status| status.phase);
        assert_eq!(first_phase, Some(rpc::AstraPhase::PhaseReady as i32));

        Ok(())
    }

    #[test]
    fn test_build_astra_config_status_rejects_vni_that_does_not_fit_i32() {
        let astra_config = rpc::AstraConfig {
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
