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

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Apply a devenv config using network segments:
    $ nico-admin-cli dev-env config apply ./devenv_config.toml --mode network-segment

Apply a devenv config using VPC prefixes:
    $ nico-admin-cli dev-env config apply ./devenv_config.toml --mode vpc-prefix

")]
pub(crate) struct Args {
    #[clap(
        help = "Path to devenv config file. Usually this is in forged repo at envs/local-dev/site/site-controller/files/generated/devenv_config.toml"
    )]
    pub(super) path: String,

    #[clap(
        long,
        short,
        help = "VPC prefix, tenant network segment, or HostInband segment?"
    )]
    pub(super) mode: NetworkChoice,
}

#[derive(ValueEnum, Parser, Debug, Clone, PartialEq)]
pub(super) enum NetworkChoice {
    NetworkSegment,
    VpcPrefix,
    /// Flat VPC plus HostInband segment for hosts with no DPU.
    HostInbandSegment,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use clap::ValueEnum;

    use super::NetworkChoice;

    #[test]
    fn network_choice_value_enum() {
        scenarios!(
            run = |value| NetworkChoice::from_str(value, false).map_err(drop);
            "network-segment" {
                "network-segment" => Yields(NetworkChoice::NetworkSegment),
            }

            "vpc-prefix" {
                "vpc-prefix" => Yields(NetworkChoice::VpcPrefix),
            }

            "host-inband-segment" {
                "host-inband-segment" => Yields(NetworkChoice::HostInbandSegment),
            }

            "invalid value" {
                "invalid" => Fails,
            }
        );
    }
}
