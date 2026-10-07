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

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[clap(group = clap::ArgGroup::new("toggle").required(true))]
#[command(after_long_help = "\
EXAMPLES:

Enable automatic machine creation:
    $ nico-admin-cli set create-machines --enable

Disable automatic machine creation:
    $ nico-admin-cli set create-machines --disable

")]
pub(crate) struct Args {
    #[clap(long, group = "toggle", help = "Enable machine creation")]
    enable: bool,

    #[clap(long, group = "toggle", help = "Disable machine creation")]
    disable: bool,
}

impl Args {
    pub(super) fn is_enabled(&self) -> bool {
        self.enable
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::Args;

    #[test]
    fn is_enabled_follows_enable_flag() {
        value_scenarios!(
            run = |args: Args| args.is_enabled();
            "enabled" {
                Args {
                    enable: true,
                    disable: false,
                } => true,
            }
            "disabled" {
                Args {
                    enable: false,
                    disable: true,
                } => false,
            }
        );
    }
}
