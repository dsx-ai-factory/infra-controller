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

use carbide_uuid::machine::MachineId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Trigger SPDM attestation for a machine with a 60-second Redfish timeout:
    $ nico-admin-cli attestation spdm trigger 12345678-1234-5678-90ab-cdef01234567 \
    --redfish-timeout-secs 60

")]
pub(crate) struct Args {
    #[clap(help = "Machine ID")]
    pub(super) machine_id: MachineId,
    #[clap(
        long,
        help = "Timeout in seconds for Redfish operations while triggering attestation (passed to carbide-api)"
    )]
    pub(super) redfish_timeout_secs: u32,
}
