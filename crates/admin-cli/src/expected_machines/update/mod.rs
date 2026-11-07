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

mod args;
mod cmd;

use std::path::Path;

pub(super) use args::Args;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::expected_machines::common::ExpectedMachineJson;

/// `expected-machine update <file>` deserializes `ExpectedMachineJson` and
/// calls `patch_expected_machine`. An omitted or `null` `interfaces` field
/// preserves the stored list, while a non-null array replaces it.
impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let json_file_path = Path::new(&self.filename);
        let file_content = std::fs::read_to_string(json_file_path)?;
        let expected_machine: ExpectedMachineJson = serde_json::from_str(&file_content)?;

        let dpu_policy = expected_machine.dpu_policy();
        let metadata = expected_machine.metadata.unwrap_or_default();
        let interfaces = expected_machine
            .interfaces
            .map(|interfaces| serde_json::to_string(&interfaces))
            .transpose()?;

        // Values present in the file replace the stored values. The patch
        // helper preserves optional fields that the file omitted.
        ctx.api_client
            .patch_expected_machine(
                Some(expected_machine.bmc_mac_address),
                None,
                Some(expected_machine.bmc_username),
                Some(expected_machine.bmc_password),
                Some(expected_machine.chassis_serial_number),
                expected_machine.fallback_dpu_serial_numbers,
                Some(metadata.name),
                Some(metadata.description),
                Some(
                    metadata
                        .labels
                        .into_iter()
                        .map(|label| {
                            if let Some(value) = label.value {
                                format!("{}:{}", label.key, value)
                            } else {
                                label.key
                            }
                        })
                        .collect(),
                ),
                expected_machine.sku_id,
                expected_machine.rack_id,
                expected_machine.default_pause_ingestion_and_poweron,
                expected_machine.dpf_enabled,
                expected_machine.bmc_ip_address,
                expected_machine.bmc_retain_credentials,
                dpu_policy,
                expected_machine.bmc_ip_allocation,
                expected_machine.host_lifecycle_profile.map(|hlp| {
                    ::rpc::forge::HostLifecycleProfile {
                        disable_lockdown: hlp.disable_lockdown,
                    }
                }),
                interfaces,
            )
            .await?;
        Ok(())
    }
}
