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

use super::args::{AgentUpgradePolicyChoice, Args};
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn agent_upgrade_policy(
    api_client: &ApiClient,
    args: Args,
) -> CarbideCliResult<()> {
    let is_set = args.set.is_some();
    let resp = api_client.0.dpu_agent_upgrade_policy_action(args).await?;
    let policy: AgentUpgradePolicyChoice = resp.active_policy.into();

    if is_set {
        tracing::info!(
            "Policy is now: {policy}. Update succeeded? {}.",
            resp.did_change
        );
    } else {
        tracing::info!("{policy}");
    }

    Ok(())
}
