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

use ::rpc::forge::BmcCredentialRotationRequest;
use ::rpc::forge::bmc_credential_rotation_request::Mode;
use carbide_uuid::device::DeviceId;
use clap::Parser;
use mac_address::MacAddress;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Force an immediate credential rotation by machine ID:
    $ nico-admin-cli credential force-bmc set --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force a switch BMC by switch ID:
    $ nico-admin-cli credential force-bmc set --id sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force a power shelf BMC (PMC) by power shelf ID:
    $ nico-admin-cli credential force-bmc set --id ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force it by BMC MAC instead (machine, switch, or power shelf):
    $ nico-admin-cli credential force-bmc set --bmc-mac 00:11:22:33:44:55

Clear a pending force-converge request:
    $ nico-admin-cli credential force-bmc clear --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

")]
pub(crate) enum Args {
    #[clap(
        about = "Request an immediate BMC credential rotation of a machine, DPU, switch, or power shelf."
    )]
    Set(ForceSet),
    #[clap(
        about = "Clear a pending BMC force-converge request for a machine, DPU, switch, or power shelf."
    )]
    Clear(ForceClear),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Force-converge a machine BMC now by machine ID:
    $ nico-admin-cli credential force-bmc set --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force-converge a switch BMC now by switch ID:
    $ nico-admin-cli credential force-bmc set --id sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force-converge a power shelf BMC (PMC) now by power shelf ID:
    $ nico-admin-cli credential force-bmc set --id ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Force-converge a BMC now by BMC MAC (machine, switch, or power shelf):
    $ nico-admin-cli credential force-bmc set --bmc-mac 00:11:22:33:44:55

")]
pub(crate) struct ForceSet {
    #[clap(
        short,
        long,
        required_unless_present_any = ["bmc_mac"],
        help = "ID of the machine, DPU, switch, or power shelf that owns the BMC. \
                Provide this or --bmc-mac."
    )]
    pub(super) id: Option<DeviceId>,

    #[clap(
        long,
        help = "MAC of the BMC to target (machine, DPU, switch, or power shelf). Provide this \
                or --id; if an id is also given they must identify the same device."
    )]
    pub(super) bmc_mac: Option<MacAddress>,
}

impl From<ForceSet> for BmcCredentialRotationRequest {
    fn from(args: ForceSet) -> Self {
        Self {
            device_id: args.id,
            mode: Mode::Set as i32,
            bmc_mac: args.bmc_mac.map(|mac| mac.to_string()),
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Clear a pending force-converge request by machine ID:
    $ nico-admin-cli credential force-bmc clear --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Clear a pending force-converge request by switch ID:
    $ nico-admin-cli credential force-bmc clear --id sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Clear a pending force-converge request by power shelf ID:
    $ nico-admin-cli credential force-bmc clear --id ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Clear a pending force-converge request by BMC MAC:
    $ nico-admin-cli credential force-bmc clear --bmc-mac 00:11:22:33:44:55

")]
pub(crate) struct ForceClear {
    #[clap(
        short,
        long,
        required_unless_present_any = ["bmc_mac"],
        help = "Machine, DPU, switch, or power shelf ID whose pending BMC force-converge request \
                should be cleared. Provide this or --bmc-mac."
    )]
    pub(super) id: Option<DeviceId>,

    #[clap(long, help = "MAC of the BMC whose pending request should be cleared.")]
    pub(super) bmc_mac: Option<MacAddress>,
}

impl From<ForceClear> for BmcCredentialRotationRequest {
    fn from(args: ForceClear) -> Self {
        Self {
            device_id: args.id,
            mode: Mode::Clear as i32,
            bmc_mac: args.bmc_mac.map(|mac| mac.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use clap::CommandFactory;

    use super::*;

    const MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
    const SWITCH_ID: &str = "sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
    const POWER_SHELF_ID: &str = "ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

    #[test]
    fn set_accepts_each_device_id_kind() {
        scenarios!(
            run = |input| {
                ForceSet::try_parse_from(["force-bmc", "--id", input])
                    .map(|args| args.id.unwrap().to_string())
                    .map_err(drop)
            };
            "device ID kinds" {
                MACHINE_ID => Yields(MACHINE_ID.to_string()),
                SWITCH_ID => Yields(SWITCH_ID.to_string()),
                POWER_SHELF_ID => Yields(POWER_SHELF_ID.to_string()),
            }

            "invalid input" {
                "not-a-device-id" => Fails,
            }
        );
    }

    #[test]
    fn set_requires_an_id_or_bmc_mac() {
        assert!(ForceSet::try_parse_from(["force-bmc"]).is_err());
    }

    #[test]
    fn power_shelf_is_a_documented_target() {
        let help = ForceSet::command().render_long_help().to_string();
        assert!(
            help.contains("power shelf"),
            "force-bmc help should document power shelf as a supported target"
        );
    }
}
