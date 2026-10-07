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
use clap::Parser;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::metadata::parse_rpc_labels;

#[derive(Parser, Debug, Serialize, Deserialize)]
#[command(after_long_help = "\
EXAMPLES:

Add an expected switch with its BMC credentials and serial number:
    $ nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB

Add an expected switch and associate it with a rack:
    $ nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB \
    --rack_id 12345678-1234-5678-90ab-cdef01234567

Add an expected switch with NVOS credentials and a static NVOS IP:
    $ nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB \
    --nvos-mac-address aa:bb:cc:dd:ee:ff --nvos-username admin --nvos-password mypassword \
    --nvos-ip-address 192.0.2.10

Add an expected switch with metadata name and a label:
    $ nico-admin-cli expected-switch add --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mypassword --switch-serial-number DGX-H100-640GB \
    --meta-name spine-01 --label DATACENTER:XYZ

")]
pub(crate) struct Args {
    #[clap(short = 'a', long, help = "BMC MAC Address of the expected switch")]
    bmc_mac_address: MacAddress,
    #[clap(short = 'u', long, help = "BMC username of the expected switch")]
    bmc_username: String,
    #[clap(short = 'p', long, help = "BMC password of the expected switch")]
    bmc_password: String,
    #[clap(
        short = 's',
        long,
        help = "Chassis serial number of the expected switch"
    )]
    switch_serial_number: String,

    #[clap(long = "nvos-mac-address", help = "NVOS MAC address(es) of the expected switch", action = clap::ArgAction::Append)]
    nvos_mac_addresses: Vec<MacAddress>,
    #[clap(long, help = "NVOS username of the expected switch")]
    nvos_username: Option<String>,
    #[clap(long, help = "NVOS password of the expected switch")]
    nvos_password: Option<String>,

    #[clap(
        long = "meta-name",
        value_name = "META_NAME",
        help = "The name that should be used as part of the Metadata for newly created Switches. If empty, the SwitchId will be used"
    )]
    meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "The description that should be used as part of the Metadata for newly created Machines"
    )]
    meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "A label that will be added as metadata for the newly created Machine. The labels key and value must be separated by a : character. E.g. DATACENTER:XYZ",
        action = clap::ArgAction::Append
    )]
    labels: Option<Vec<String>>,

    #[clap(
        long = "rack_id",
        value_name = "RACK_ID",
        help = "Rack ID for this machine",
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
        help = "Static IP for the single wired NVOS port. Requires exactly one --nvos-mac-address"
    )]
    nvos_ip_address: Option<IpAddr>,

    #[clap(
        long = "bmc-retain-credentials",
        value_name = "BMC_RETAIN_CREDENTIALS",
        help = "When true, site-explorer skips BMC password rotation and stores factory-default credentials in Vault as-is"
    )]
    bmc_retain_credentials: Option<bool>,
}

impl From<Args> for rpc::forge::ExpectedSwitch {
    fn from(value: Args) -> Self {
        let labels = parse_rpc_labels(value.labels.unwrap_or_default());
        let metadata = rpc::forge::Metadata {
            name: value.meta_name.unwrap_or_default(),
            description: value.meta_description.unwrap_or_default(),
            labels,
        };
        Self {
            expected_switch_id: None,
            bmc_mac_address: value.bmc_mac_address.to_string(),
            bmc_username: value.bmc_username,
            bmc_password: value.bmc_password,
            switch_serial_number: value.switch_serial_number,
            metadata: Some(metadata),
            rack_id: value.rack_id,
            nvos_mac_addresses: value
                .nvos_mac_addresses
                .iter()
                .map(|m| m.to_string())
                .collect(),
            nvos_username: value.nvos_username,
            nvos_password: value.nvos_password,
            bmc_ip_address: value
                .bmc_ip_address
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            nvos_ip_address: value.nvos_ip_address.map(|ip| ip.to_string()),
            bmc_retain_credentials: value.bmc_retain_credentials,
        }
    }
}
