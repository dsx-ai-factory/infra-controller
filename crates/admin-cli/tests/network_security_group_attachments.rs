/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{
    FindNetworkSecurityGroupsByIdsRequest, FindNetworkSecurityGroupsByIdsResponse,
    GetNetworkSecurityGroupAttachmentsRequest, GetNetworkSecurityGroupAttachmentsResponse,
    GetNetworkSecurityGroupPropagationStatusRequest,
    GetNetworkSecurityGroupPropagationStatusResponse, NetworkSecurityGroup,
    NetworkSecurityGroupAttachments, NetworkSecurityGroupIdList,
    NetworkSecurityGroupPropagationObjectStatus, NetworkSecurityGroupPropagationStatus,
};
use support::{Exchange, assert_success, run_command};

const NSG_ID: &str = "12345678-1234-5678-90ab-cdef01234567";
const VPC_ID: &str = "abcdef01-2345-6789-abcd-ef0123456789";
const INCOMPLETE_INSTANCE_ID: &str = "200f1043-1653-426d-bd0e-97f5b06bdb3f";
const COMPLETE_INSTANCE_ID: &str = "fb02b51c-3f18-46b8-b2f1-bc4a6e9b2f3d";

/// Shows an indirect instance in `unpropagated_instance_ids` as Incomplete so missing evidence
/// cannot be mistaken for a claim that no interfaces have applied policy.
#[tokio::test]
async fn show_attachments_marks_indirect_instances_with_missing_evidence_incomplete() {
    // Include one related instance in the unpropagated list and omit the other.
    // The NSG fields supply populated and empty cells in the same command output.
    let propagation = NetworkSecurityGroupPropagationObjectStatus {
        id: VPC_ID.to_string(),
        status: NetworkSecurityGroupPropagationStatus::NsgPropStatusPartial.into(), // Some interfaces applied.
        related_instance_ids: vec![
            INCOMPLETE_INSTANCE_ID.to_string(),
            COMPLETE_INSTANCE_ID.to_string(),
        ], // Render both incomplete and complete indirect instances.
        unpropagated_instance_ids: vec![INCOMPLETE_INSTANCE_ID.to_string()], // Some evidence is missing.
        ..Default::default()
    };

    // Run the public command through its NSG, attachment, and propagation RPCs.
    let output = run_command(
        &[
            "network-security-group",
            "show-attachments",
            "--id",
            NSG_ID,
            "--include-indirect",
        ],
        None,
        vec![
            Exchange {
                method: rpc::service_path!("FindNetworkSecurityGroupsByIds"),
                request: FindNetworkSecurityGroupsByIdsRequest {
                    network_security_group_ids: vec![NSG_ID.to_string()],
                    tenant_organization_id: None,
                }
                .encode_to_vec(),
                response: Ok(FindNetworkSecurityGroupsByIdsResponse {
                    network_security_groups: vec![NetworkSecurityGroup {
                        id: NSG_ID.to_string(),
                        tenant_organization_id: "Tenant1".to_string(),
                        version: "V1-T1720000000000000".to_string(),
                        ..Default::default() // Leave optional NSG cells empty.
                    }],
                }
                .encode_to_vec()),
            },
            Exchange {
                method: rpc::service_path!("GetNetworkSecurityGroupAttachments"),
                request: GetNetworkSecurityGroupAttachmentsRequest {
                    network_security_group_ids: vec![NSG_ID.to_string()],
                }
                .encode_to_vec(),
                response: Ok(GetNetworkSecurityGroupAttachmentsResponse {
                    attachments: vec![NetworkSecurityGroupAttachments {
                        network_security_group_id: NSG_ID.to_string(),
                        vpc_ids: vec![VPC_ID.to_string()],
                        instance_ids: vec![],
                    }],
                }
                .encode_to_vec()),
            },
            Exchange {
                method: rpc::service_path!("GetNetworkSecurityGroupPropagationStatus"),
                request: GetNetworkSecurityGroupPropagationStatusRequest {
                    network_security_group_ids: Some(NetworkSecurityGroupIdList {
                        ids: vec![NSG_ID.to_string()],
                    }),
                    vpc_ids: vec![VPC_ID.to_string()],
                    instance_ids: vec![],
                }
                .encode_to_vec(),
                response: Ok(GetNetworkSecurityGroupPropagationStatusResponse {
                    vpcs: vec![propagation],
                    instances: vec![],
                }
                .encode_to_vec()),
            },
        ],
    )
    .await;

    // Check the command result and all rendered cells, including empty NSG values.
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 attachment tables");
    let rows: Vec<Vec<_>> = stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect())
        .collect();
    let relationship = format!("INDIRECT via VPC {VPC_ID}");
    assert_eq!(
        rows,
        [
            vec![
                "Id",
                "Tenant Organization ID",
                "Name",
                "Description",
                "Version",
                "Created",
                "Created By",
                "Updated By",
                "Labels",
            ],
            vec![
                NSG_ID,
                "Tenant1",
                "",
                "",
                "V1-T1720000000000000",
                "",
                "",
                "",
                ""
            ],
            vec!["Id", "Type"],
            vec![VPC_ID, "VPC"],
            vec!["Id", "Type", "Relationship", "Propagated"],
            vec![VPC_ID, "VPC", "DIRECT", "NSG_PROP_STATUS_PARTIAL"],
            vec![
                INCOMPLETE_INSTANCE_ID,
                "INSTANCE",
                &relationship,
                "Incomplete"
            ],
            vec![
                COMPLETE_INSTANCE_ID,
                "INSTANCE",
                &relationship,
                "NSG_PROP_STATUS_FULL",
            ],
        ],
        "{stdout}",
    );
}
