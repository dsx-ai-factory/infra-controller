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
use carbide_uuid::instance_type::InstanceTypeId;
use model::machine::machine_search_config::MachineSearchConfig;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl TryFrom<rpc::forge::MachineSearchConfig> for MachineSearchConfig {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::MachineSearchConfig) -> Result<Self, Self::Error> {
        Ok(MachineSearchConfig {
            include_dpus: value.include_dpus,
            include_history: value.include_history,
            include_predicted_host: value.include_predicted_host,
            only_maintenance: value.only_maintenance,
            only_quarantine: value.only_quarantine,
            exclude_hosts: value.exclude_hosts,
            instance_type_id: value
                .instance_type_id
                .map(|t| {
                    t.parse::<InstanceTypeId>()
                        .map_err(|_| RpcDataConversionError::InvalidInstanceTypeId(t.clone()))
                })
                .transpose()?,
            for_update: false, // This isn't exposed to API callers
            mnnvl_only: value.mnnvl_only,
            only_with_power_state: value.only_with_power_state,
            only_with_health_alert: value.only_with_health_alert,
            rack_id: value.rack_id,
            controller_state: None,
        })
    }
}
