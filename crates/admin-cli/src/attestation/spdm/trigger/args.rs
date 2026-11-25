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
use regex::Regex;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Trigger SPDM attestation for a machine with a 60-second Redfish timeout:
    $ nico-admin-cli attestation spdm trigger fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg \
    --redfish-timeout-secs 60

Attest eligible GPUs, overriding the machine's profile selection:
    $ nico-admin-cli attestation spdm trigger fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg \
    --redfish-timeout-secs 60 --attester-id-regex 'GPU_'

Attest exactly one reported attester ID:
    $ nico-admin-cli attestation spdm trigger fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg \
    --redfish-timeout-secs 60 --attester-id-regex '^HGX_IRoT_GPU_0$'

")]
pub(crate) struct Args {
    #[clap(help = "Machine ID")]
    pub(super) machine_id: MachineId,
    #[clap(
        long,
        help = "Timeout in seconds for Redfish operations while triggering attestation (passed to carbide-api)"
    )]
    pub(super) redfish_timeout_secs: u32,
    #[clap(
        long,
        value_name = "REGEX",
        value_parser = parse_attester_id_regex,
        help = "Override profile selection with a regex against reported attester IDs",
        long_help = "Override profile selection, including mode NONE or a missing profile, for this request only. \
                     Uses Rust regex syntax; matching is case-sensitive by default and searches within each \
                     reported attester ID unless anchored with ^ and $. Device eligibility checks and \
                     site-wide SPDM enablement still apply. Empty or invalid patterns are rejected. \
                     If no eligible attester matches, no job is created. \
                     Omit this option to use the profile for this machine."
    )]
    pub(super) attester_id_regex: Option<Regex>,
}

fn parse_attester_id_regex(pattern: &str) -> Result<Regex, String> {
    // Empty regexes match every ID, so require an explicit pattern such as .* for that intent.
    if pattern.is_empty() {
        return Err("attester ID regex must not be empty".to_string());
    }

    // Parse here so invalid invocations retain Clap's usage output and exit status.
    Regex::new(pattern).map_err(|error| format!("invalid attester ID regex: {error}"))
}
