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

use model::machine::spx::{MachineSpxAttachmentStatusObservation, MachineSpxStatusObservation};

use crate::forge as rpc;

impl From<MachineSpxStatusObservation> for rpc::MachineSpxStatusObservation {
    fn from(value: MachineSpxStatusObservation) -> Self {
        rpc::MachineSpxStatusObservation {
            attachment_status: value
                .spx_attachments
                .into_iter()
                .map(rpc::MachineSpxAttachmentStatusObservation::from)
                .collect(),
            observed_at: Some(value.observed_at.into()),
        }
    }
}

impl From<MachineSpxAttachmentStatusObservation> for rpc::MachineSpxAttachmentStatusObservation {
    fn from(value: MachineSpxAttachmentStatusObservation) -> Self {
        rpc::MachineSpxAttachmentStatusObservation {
            mac_address: value.mac_address.to_string(),
            partition_id: value.partition_id,
            attachment_type: value.attachment_type.map(|at| at as i32),
            virtual_function_id: value.virtual_function_id,
            observed_at: Some(value.observed_at.into()),
        }
    }
}
