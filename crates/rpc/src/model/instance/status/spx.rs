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

use model::instance::config::spx::SpxAttachmentType;
use model::instance::status::spx::{
    InstanceSpxAttachmentStatus, InstanceSpxStatus, SpxAttachmentVfStatus,
};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<InstanceSpxStatus> for rpc::InstanceSpxStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceSpxStatus) -> Result<Self, Self::Error> {
        let mut spx_attachments: Vec<rpc::InstanceSpxAttachmentStatus> = Vec::new();
        for attachment in status.spx_attachments.iter() {
            let a = rpc::InstanceSpxAttachmentStatus::try_from(attachment.clone())?;
            spx_attachments.push(a);
        }
        Ok(Self {
            attachment_statuses: spx_attachments,
            configs_synced: rpc::SyncState::try_from(status.configs_synced)? as i32,
        })
    }
}

impl TryFrom<InstanceSpxAttachmentStatus> for rpc::InstanceSpxAttachmentStatus {
    type Error = RpcDataConversionError;
    fn try_from(status: InstanceSpxAttachmentStatus) -> Result<Self, Self::Error> {
        Ok(Self {
            mac_addr: Some(status.mac_address),
            virtual_function_id: status
                .attachment_vf
                .map(|vf| vf.vf_index)
                .unwrap_or_default(),
            attachment_type: status.attachment_type as i32,
            spx_partition_id: Some(status.spx_partition_id),
            ip_address: None,
        })
    }
}

impl TryFrom<rpc::InstanceSpxAttachmentStatus> for InstanceSpxAttachmentStatus {
    type Error = RpcDataConversionError;
    fn try_from(status: rpc::InstanceSpxAttachmentStatus) -> Result<Self, Self::Error> {
        let attachment_type =
            SpxAttachmentType::try_from(status.attachment_type).map_err(|_| {
                RpcDataConversionError::InvalidValue(
                    "SpxAttachmentType".to_string(),
                    status.attachment_type.to_string(),
                )
            })?;
        Ok(Self {
            mac_address: status.mac_addr.unwrap_or_default(),
            attachment_type,
            spx_partition_id: status.spx_partition_id.unwrap_or_default(),
            attachment_vf: Some(SpxAttachmentVfStatus {
                vf_index: status.virtual_function_id,
            }),
            attachment_ovs: None,
        })
    }
}
