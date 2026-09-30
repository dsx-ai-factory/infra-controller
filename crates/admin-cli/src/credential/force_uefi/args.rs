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

use ::rpc::forge::UefiCredentialRotationRequest;
use ::rpc::forge::uefi_credential_rotation_request::Mode;
use carbide_uuid::machine::MachineId;
use clap::Parser;
use mac_address::MacAddress;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Force an immediate UEFI credential rotation by machine ID:
    $ nico-admin-cli credential force-uefi set --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force it by BMC MAC instead:
    $ nico-admin-cli credential force-uefi set --bmc-mac 00:11:22:33:44:55

Clear a pending force-converge request:
    $ nico-admin-cli credential force-uefi clear --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

")]
pub(crate) enum Args {
    #[clap(about = "Request an immediate UEFI credential rotation of a machine (host).")]
    Set(ForceSet),
    #[clap(about = "Clear a pending UEFI force-converge request for a machine (host).")]
    Clear(ForceClear),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Force-converge a machine's UEFI credential now by machine ID:
    $ nico-admin-cli credential force-uefi set --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force-converge by BMC MAC:
    $ nico-admin-cli credential force-uefi set --bmc-mac 00:11:22:33:44:55

")]
pub(crate) struct ForceSet {
    #[clap(
        short,
        long,
        required_unless_present = "bmc_mac",
        help = "Machine ID that owns the UEFI credential (a host machine). \
                Provide this or --bmc-mac."
    )]
    pub(super) id: Option<MachineId>,

    #[clap(
        long,
        help = "MAC of the BMC for the machine. Provide this or --id; if both are given \
                they must identify the same machine."
    )]
    pub(super) bmc_mac: Option<MacAddress>,
}

impl From<ForceSet> for UefiCredentialRotationRequest {
    fn from(args: ForceSet) -> Self {
        Self {
            machine_id: args.id,
            mode: Mode::Set as i32,
            bmc_mac: args.bmc_mac.map(|mac| mac.to_string()),
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Clear a pending force-converge request by machine ID:
    $ nico-admin-cli credential force-uefi clear --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Clear a pending force-converge request by BMC MAC:
    $ nico-admin-cli credential force-uefi clear --bmc-mac 00:11:22:33:44:55

")]
pub(crate) struct ForceClear {
    #[clap(
        short,
        long,
        required_unless_present = "bmc_mac",
        help = "Machine ID whose pending UEFI force-converge request should be cleared. \
                Provide this or --bmc-mac."
    )]
    pub(super) id: Option<MachineId>,

    #[clap(
        long,
        help = "MAC of the BMC for the machine whose pending request should be cleared."
    )]
    pub(super) bmc_mac: Option<MacAddress>,
}

impl From<ForceClear> for UefiCredentialRotationRequest {
    fn from(args: ForceClear) -> Self {
        Self {
            machine_id: args.id,
            mode: Mode::Clear as i32,
            bmc_mac: args.bmc_mac.map(|mac| mac.to_string()),
        }
    }
}
