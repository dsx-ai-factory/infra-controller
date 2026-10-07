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

#[derive(clap::Subcommand, Debug)]
#[clap(rename_all = "kebab-case")]
#[command(after_long_help = "\
EXAMPLES:

Print network status of all DPUs:
    $ nico-admin-cli dpu network status

Show the VPC network configuration for one DPU:
    $ nico-admin-cli dpu network config --machine-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum Args {
    #[clap(about = "Print network status of all machines")]
    Status,
    #[clap(about = "Machine network configuration, used by VPC.")]
    #[command(after_long_help = "\
EXAMPLES:

Show the VPC network configuration for one DPU:
    $ nico-admin-cli dpu network config --machine-id 12345678-1234-5678-90ab-cdef01234567

")]
    Config(crate::machine::NetworkConfigQuery),
}
