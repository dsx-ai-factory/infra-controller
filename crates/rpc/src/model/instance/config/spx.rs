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

use model::instance::config::spx::{
    InstanceSpxAttachment, InstanceSpxConfig, SpxAttachmentOvs, SpxAttachmentType, SpxAttachmentVf,
};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<rpc::InstanceSpxConfig> for InstanceSpxConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::InstanceSpxConfig) -> Result<Self, Self::Error> {
        let mut spx_attachments = Vec::with_capacity(config.spx_attachments.len());
        for attachment in config.spx_attachments.into_iter() {
            let spx_partition_id =
                attachment
                    .spx_partition_id
                    .ok_or(RpcDataConversionError::MissingArgument(
                        "InstanceSpxAttachment::spx_partition_id",
                    ))?;
            let attachment_type =
                SpxAttachmentType::try_from(attachment.attachment_type).map_err(|_| {
                    RpcDataConversionError::InvalidValue(
                        "SpxAttachmentType".to_string(),
                        attachment.attachment_type.to_string(),
                    )
                })?;
            spx_attachments.push(InstanceSpxAttachment {
                device: attachment.device,
                device_instance: attachment.device_instance,
                spx_partition_id,
                attachment_type,
                attachment_vf: attachment.attachment_vf.map(|vf| SpxAttachmentVf {
                    vf_index: vf.vf_index,
                }),
                attachment_ovs: attachment.attachment_ovs.map(|ovs| SpxAttachmentOvs {
                    bridge_name: ovs.bridge_name,
                    ovn_network_name: ovs.ovn_network_name,
                }),
                mac_address: None,
            });
        }
        Ok(Self { spx_attachments })
    }
}

impl TryFrom<InstanceSpxConfig> for rpc::InstanceSpxConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: InstanceSpxConfig) -> Result<rpc::InstanceSpxConfig, Self::Error> {
        let mut spx_attachments = Vec::with_capacity(config.spx_attachments.len());
        for attachment in config.spx_attachments.into_iter() {
            spx_attachments.push(rpc::InstanceSpxAttachment {
                device: attachment.device,
                device_instance: attachment.device_instance,
                spx_partition_id: Some(attachment.spx_partition_id),
                attachment_type: attachment.attachment_type as i32,
                attachment_vf: attachment.attachment_vf.map(|vf| rpc::SpxAttchmentVf {
                    vf_index: vf.vf_index,
                }),
                attachment_ovs: attachment.attachment_ovs.map(|ovs| rpc::SpxAttchmentOvs {
                    bridge_name: ovs.bridge_name,
                    ovn_network_name: ovs.ovn_network_name,
                }),
            });
        }
        Ok(rpc::InstanceSpxConfig { spx_attachments })
    }
}
