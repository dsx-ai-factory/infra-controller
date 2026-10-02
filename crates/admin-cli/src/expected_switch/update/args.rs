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

use std::net::IpAddr;

use carbide_uuid::rack::RackId;
use clap::error::ErrorKind;
use clap::{ArgGroup, CommandFactory, Parser};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Update an expected switch.
///
/// Select the switch by either BMC MAC address or ID. Supply at least one update field.
/// Omitted fields and empty metadata names or descriptions remain unchanged. Supplied labels
/// replace the whole label collection. Supplied NVOS MAC addresses replace the stored list.
///
/// Supply a username, password, or both for BMC or NVOS. Each omitted credential field keeps its
/// stored value. NVOS credentials must contain both values after the update, so configuring them
/// for the first time requires both flags. Empty BMC values leave their fields unchanged; Core
/// PATCH rejects empty NVOS values. Legacy fallback uses the validation rules on the older server.
///
/// The command first tries Core PATCH, which merges selected fields atomically. It falls back to
/// the legacy update on `Unimplemented` or `PermissionDenied`, or when a MAC lookup returns no ID.
/// Legacy fallback preserves omitted fields and accepts either selector when Core supports the
/// switch update mask introduced in https://github.com/dsx-ai-factory/infra-controller/pull/3706.
/// Earlier servers use a full replacement: select by BMC MAC address and supply every value you
/// need to preserve. The legacy request still requires authorization. Other PATCH errors and
/// failed legacy updates remain errors.
///
/// https://github.com/dsx-ai-factory/infra-controller/pull/6359
#[derive(Parser, Debug, Serialize, Deserialize)]
#[clap(verbatim_doc_comment)]
#[command(after_long_help = "\
EXAMPLES:

Correct a switch's BMC username while preserving the password:
    $ nico-admin-cli expected-switch update --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin

Update an expected switch's serial number, selecting it by ID:
    $ nico-admin-cli expected-switch update --id 12345678-1234-5678-90ab-cdef01234567 \
    --switch-serial-number DGX-H100-640GB

Correct the password of existing NVOS credentials while preserving the username:
    $ nico-admin-cli expected-switch update --bmc-mac-address 00:11:22:33:44:55 \
    --nvos-password mynewpassword

Configure both NVOS credential fields:
    $ nico-admin-cli expected-switch update --bmc-mac-address 00:11:22:33:44:55 \
    --nvos-username admin --nvos-password mynewpassword

")]
#[clap(group(ArgGroup::new("group").required(true).multiple(true).args(&[
"bmc_username",
"bmc_password",
"switch_serial_number",
"nvos_mac_addresses",
"nvos_username",
"nvos_password",
"meta_name",
"meta_description",
"labels",
"rack_id",
"bmc_ip_address",
"nvos_ip_address",
"bmc_retain_credentials",
])))]
pub(crate) struct Args {
    #[clap(short = 'a', long, help = "BMC MAC Address of the expected switch")]
    bmc_mac_address: Option<MacAddress>,

    #[clap(long = "id", help = "ID (UUID) of the expected switch to update.")]
    #[serde(skip)]
    id: Option<Uuid>,
    #[clap(
        short = 'u',
        long,
        group = "group",
        help = "BMC username of the expected switch"
    )]
    bmc_username: Option<String>,
    #[clap(
        short = 'p',
        long,
        group = "group",
        help = "BMC password of the expected switch"
    )]
    bmc_password: Option<String>,
    #[clap(
        short = 's',
        long,
        group = "group",
        help = "Switch serial number of the expected switch"
    )]
    switch_serial_number: Option<String>,

    #[clap(
        long = "nvos-mac-address",
        group = "group",
        help = "NVOS MAC address(es) of the expected switch",
        action = clap::ArgAction::Append
    )]
    nvos_mac_addresses: Vec<MacAddress>,
    #[clap(long, group = "group", help = "NVOS username of the expected switch")]
    nvos_username: Option<String>,
    #[clap(long, group = "group", help = "NVOS password of the expected switch")]
    nvos_password: Option<String>,

    #[clap(
        long = "meta-name",
        value_name = "META_NAME",
        help = "Replace the metadata name. An empty or omitted value leaves it unchanged"
    )]
    meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "Replace the metadata description. An empty or omitted value leaves it unchanged"
    )]
    meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "Replace all metadata labels with the supplied key or key:value entries. Repeat for each label. Duplicate keys are rejected; label order is not preserved. Omission preserves labels",
        action = clap::ArgAction::Append
    )]
    labels: Option<Vec<String>>,

    #[clap(
        long = "rack_id",
        value_name = "RACK_ID",
        help = "Rack ID for this switch",
        action = clap::ArgAction::Append
    )]
    rack_id: Option<RackId>,

    #[clap(
        long = "bmc-ip-address",
        value_name = "BMC_IP_ADDRESS",
        help = "BMC IP address of the expected switch"
    )]
    bmc_ip_address: Option<IpAddr>,

    #[clap(
        long = "nvos-ip-address",
        value_name = "NVOS_IP_ADDRESS",
        help = "Static IP for the single wired NVOS port. The updated switch must have exactly one NVOS MAC address",
        long_help = "Static IP for the single wired NVOS port. The updated switch must have exactly one NVOS MAC address. When Core supports PATCH or masked updates, omit --nvos-mac-address only if the stored list already contains exactly one MAC; otherwise, supply exactly one --nvos-mac-address to replace the list. Older servers that support NVOS IPs but replace the full record require exactly one --nvos-mac-address in this command; omitted fields can be cleared. Servers without NVOS IP support ignore this field"
    )]
    nvos_ip_address: Option<IpAddr>,

    #[clap(
        long = "bmc-retain-credentials",
        value_name = "BMC_RETAIN_CREDENTIALS",
        help = "When true, site-explorer skips BMC password rotation and stores factory-default credentials in Vault as-is"
    )]
    bmc_retain_credentials: Option<bool>,
}

impl Args {
    pub(super) fn validate(&self) -> Result<(), clap::Error> {
        let error = |kind, message: &str| {
            Self::command()
                .bin_name("nico-admin-cli expected-switch update")
                .error(kind, message)
        };
        match (&self.bmc_mac_address, &self.id) {
            (Some(_), Some(_)) => Err(error(
                ErrorKind::ArgumentConflict,
                "cannot specify both --bmc-mac-address and --id; provide only one",
            )),
            (None, None) => Err(error(
                ErrorKind::MissingRequiredArgument,
                "must specify either --bmc-mac-address or --id",
            )),
            _ => Ok(()),
        }
    }
}

impl From<Args> for rpc::forge::ExpectedSwitch {
    fn from(args: Args) -> Self {
        Self {
            expected_switch_id: args.id.map(|id| ::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: args
                .bmc_mac_address
                .map(|m| m.to_string())
                .unwrap_or_default(),
            bmc_username: args.bmc_username.unwrap_or_default(),
            bmc_password: args.bmc_password.unwrap_or_default(),
            switch_serial_number: args.switch_serial_number.unwrap_or_default(),
            nvos_username: args.nvos_username,
            nvos_password: args.nvos_password,
            metadata: Some(rpc::forge::Metadata {
                name: args.meta_name.unwrap_or_default(),
                description: args.meta_description.unwrap_or_default(),
                labels: crate::metadata::parse_rpc_labels(args.labels.unwrap_or_default()),
            }),
            rack_id: args.rack_id,
            nvos_mac_addresses: args
                .nvos_mac_addresses
                .iter()
                .map(|m| m.to_string())
                .collect(),
            bmc_ip_address: args
                .bmc_ip_address
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            nvos_ip_address: args.nvos_ip_address.map(|ip| ip.to_string()),
            bmc_retain_credentials: args.bmc_retain_credentials,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::scenarios;
    use rpc::forge_api_client::{ExpectedSwitchUpdateField as Field, expected_switch_update_mask};

    use super::*;
    use crate::cfg::cli_options::{CliCommand, CliOptions};
    use crate::expected_switch::Cmd;

    #[test]
    fn standalone_fields_select_only_the_requested_update() {
        let parse_update = |fields: &[&str]| {
            let options = CliOptions::try_parse_from(
                [
                    "nico-admin-cli",
                    "expected-switch",
                    "update",
                    "--id",
                    "12345678-1234-5678-90ab-cdef01234567",
                ]
                .into_iter()
                .chain(fields.iter().copied()),
            )
            .map_err(|error| error.kind())?;
            let Some(CliCommand::ExpectedSwitch(Cmd::Update(args))) = options.commands else {
                panic!("expected switch update command");
            };
            Ok(expected_switch_update_mask(&args.into()))
        };

        scenarios!(parse_update:
            "standalone update fields" {
                ["--bmc-username", "replacement"].as_slice() => Yields(vec![Field::BmcUsername]),
                ["--bmc-password", "replacement"].as_slice() => Yields(vec![Field::BmcPassword]),
                ["--bmc-ip-address", "192.0.2.10"].as_slice() => Yields(vec![Field::BmcIpAddress]),
                ["--nvos-ip-address", "192.0.2.20"].as_slice() => Yields(vec![Field::NvosIpAddress]),
                ["--rack_id", "12345678-1234-5678-90ab-cdef01234567"].as_slice() => Yields(vec![Field::RackId]),
                ["--bmc-retain-credentials", "false"].as_slice() => Yields(vec![Field::BmcRetainCredentials]),
                ["--meta-name", "switch-1"].as_slice() => Yields(vec![Field::MetadataName]),
                ["--meta-description", "rack switch"].as_slice() => Yields(vec![Field::MetadataDescription]),
                ["--label", "rack:r1"].as_slice() => Yields(vec![Field::MetadataLabels]),
            }

            "an update field is required" {
                [].as_slice() => FailsWith(ErrorKind::MissingRequiredArgument),
            }
        );
    }
}
