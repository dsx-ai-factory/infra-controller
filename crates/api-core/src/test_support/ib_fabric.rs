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

use std::sync::Arc;

use carbide_ib_fabric::config::IBFabricConfig;
use carbide_ib_fabric::ib::{self, IBFabricManagerImpl, IBFabricManagerType};
use carbide_secrets::credentials::CredentialManager;

use crate::cfg::file::CarbideConfig;

pub(crate) fn ib_fabric_test_manager(
    config: &CarbideConfig,
    credential_manager: Arc<dyn CredentialManager>,
) -> Arc<IBFabricManagerImpl> {
    let ib_config = config.ib_config.clone().unwrap_or_default();
    ib::create_ib_fabric_manager(
        credential_manager,
        ib::IBFabricManagerConfig {
            allow_insecure_fabric_configuration: ib_config.allow_insecure,
            endpoints: if ib_config.enabled {
                config
                    .ib_fabrics
                    .iter()
                    .map(|(fabric_id, fabric_definition)| {
                        (fabric_id.clone(), fabric_definition.endpoints.clone())
                    })
                    .collect()
            } else {
                Default::default()
            },
            manager_type: if ib_config.enabled {
                IBFabricManagerType::Mock
            } else {
                IBFabricManagerType::Disable
            },
            fabric_manager_run_interval: std::time::Duration::from_secs(10),
            max_partition_per_tenant: IBFabricConfig::default_max_partition_per_tenant(),
            mtu: ib_config.mtu,
            rate_limit: ib_config.rate_limit,
            service_level: ib_config.service_level,
        },
    )
    .unwrap()
    .into()
}
