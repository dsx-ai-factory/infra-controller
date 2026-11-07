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

use model::machine::infiniband::{
    MachineIbInterfaceStatusObservation, MachineInfinibandStatusObservation,
};

use crate as rpc;

impl From<MachineInfinibandStatusObservation> for rpc::forge::InfinibandStatusObservation {
    fn from(
        ib_status: MachineInfinibandStatusObservation,
    ) -> rpc::forge::InfinibandStatusObservation {
        rpc::forge::InfinibandStatusObservation {
            ib_interfaces: ib_status
                .ib_interfaces
                .into_iter()
                .map(|interface| interface.into())
                .collect(),
            observed_at: Some(ib_status.observed_at.into()),
        }
    }
}

impl From<MachineIbInterfaceStatusObservation> for rpc::forge::MachineIbInterface {
    fn from(
        machine_ib_interface: MachineIbInterfaceStatusObservation,
    ) -> rpc::forge::MachineIbInterface {
        rpc::forge::MachineIbInterface {
            pf_guid: None,
            guid: Some(machine_ib_interface.guid),
            lid: Some(machine_ib_interface.lid as u32),
            fabric_id: match machine_ib_interface.fabric_id.is_empty() {
                true => None,
                false => Some(machine_ib_interface.fabric_id),
            },
            associated_pkeys: machine_ib_interface.associated_pkeys.map(|pkeys| {
                rpc::common::StringList {
                    items: pkeys.into_iter().map(|key| key.to_string()).collect(),
                }
            }),
            associated_partition_ids: machine_ib_interface.associated_partition_ids.map(|ids| {
                rpc::common::StringList {
                    items: ids.into_iter().map(|id| id.into()).collect(),
                }
            }),
        }
    }
}
