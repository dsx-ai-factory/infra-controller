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

use super::types::{FabricRead, PortData, PortName};

impl FabricRead<'_> {
    pub(crate) fn ports(&self) -> Vec<PortData<'_>> {
        let mut ports = self
            .state
            .ports
            .iter()
            .map(|(guid, port)| {
                let system_name = port
                    .candidate
                    .machine_id
                    .as_ref()
                    .map(AsRef::as_ref)
                    .unwrap_or_else(|| port.candidate.mat_id.as_ref());
                PortData {
                    guid,
                    name: PortName(guid),
                    system_id: guid,
                    lid: &port.lid,
                    dname: port.candidate.mat_id.as_ref(),
                    system_name,
                    physical_state: port.candidate.state.physical_state(),
                    logical_state: port.candidate.state.logical_state(),
                }
            })
            .collect::<Vec<_>>();
        ports.sort_by(|left, right| left.guid.cmp(right.guid));
        ports
    }
}
