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

pub(super) use args::Args;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

/// `expected-machine patch`: forwards CLI flags to `ApiClient::patch_expected_machine` (partial
/// update; unset flags keep existing values).
impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        if let Err(error) = self.validate() {
            error.exit();
        }
        ctx.api_client
            .patch_expected_machine(
                self.bmc_mac_address,
                self.id.map(|id| id.to_string()),
                self.bmc_username,
                self.bmc_password,
                self.chassis_serial_number,
                self.fallback_dpu_serial_numbers,
                self.meta_name,
                self.meta_description,
                self.labels,
                self.sku_id,
                self.rack_id,
                self.default_pause_ingestion_and_poweron,
                self.dpf_enabled,
                self.bmc_ip_address,
                self.bmc_retain_credentials,
                self.dpu_policy,
                self.bmc_ip_allocation,
                self.disable_lockdown
                    .map(|dl| ::rpc::forge::HostLifecycleProfile {
                        disable_lockdown: Some(dl),
                    }),
                self.interfaces,
            )
            .await?;
        Ok(())
    }
}
