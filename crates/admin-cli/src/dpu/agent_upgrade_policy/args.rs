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

use clap::{Parser, ValueEnum};
use rpc::forge::DpuAgentUpgradePolicyRequest;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show the current forge-dpu-agent upgrade policy:
    $ nico-admin-cli dpu agent-upgrade-policy

Allow the agent to upgrade only (never downgrade):
    $ nico-admin-cli dpu agent-upgrade-policy --set up-only

Allow the agent to both upgrade and downgrade:
    $ nico-admin-cli dpu agent-upgrade-policy --set up-down

Disable automatic agent version changes:
    $ nico-admin-cli dpu agent-upgrade-policy --set off

")]
pub(crate) struct Args {
    #[clap(long)]
    pub(super) set: Option<AgentUpgradePolicyChoice>,
}

impl From<Args> for DpuAgentUpgradePolicyRequest {
    fn from(args: Args) -> Self {
        Self {
            new_policy: args.set.map(|choice| match choice {
                AgentUpgradePolicyChoice::Off => rpc::forge::AgentUpgradePolicy::Off as i32,
                AgentUpgradePolicyChoice::UpOnly => rpc::forge::AgentUpgradePolicy::UpOnly as i32,
                AgentUpgradePolicyChoice::UpDown => rpc::forge::AgentUpgradePolicy::UpDown as i32,
            }),
        }
    }
}

// Should match api/src/model/machine/upgrade_policy.rs AgentUpgradePolicy
#[derive(ValueEnum, Debug, Clone)]
pub(super) enum AgentUpgradePolicyChoice {
    Off,
    UpOnly,
    UpDown,
}

impl std::fmt::Display for AgentUpgradePolicyChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // enums are a special case where their debug impl is their name ("Off")
        std::fmt::Debug::fmt(self, f)
    }
}

// From the RPC
impl From<i32> for AgentUpgradePolicyChoice {
    fn from(rpc_policy: i32) -> Self {
        use rpc::forge::AgentUpgradePolicy::*;
        match rpc_policy {
            n if n == Off as i32 => AgentUpgradePolicyChoice::Off,
            n if n == UpOnly as i32 => AgentUpgradePolicyChoice::UpOnly,
            n if n == UpDown as i32 => AgentUpgradePolicyChoice::UpDown,
            _ => {
                unreachable!();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use clap::ValueEnum;

    use super::AgentUpgradePolicyChoice;

    #[test]
    fn agent_upgrade_policy_choice_value_enum() {
        scenarios!(
            run = |value| {
                AgentUpgradePolicyChoice::from_str(value, false)
                    .map(|choice| match choice {
                        AgentUpgradePolicyChoice::Off => "off",
                        AgentUpgradePolicyChoice::UpOnly => "up-only",
                        AgentUpgradePolicyChoice::UpDown => "up-down",
                    })
                    .map_err(drop)
            };
            "off" {
                "off" => Yields("off"),
            }

            "up-only" {
                "up-only" => Yields("up-only"),
            }

            "up-down" {
                "up-down" => Yields("up-down"),
            }

            "invalid value" {
                "invalid" => Fails,
            }
        );
    }
}
