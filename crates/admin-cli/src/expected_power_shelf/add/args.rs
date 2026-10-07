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

Add an expected power shelf with its BMC credentials and serial number:
    $ nico-admin-cli expected-power-shelf add --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mypassword --shelf-serial-number DGX-H100-640GB

Add an expected power shelf and associate it with a rack:
    $ nico-admin-cli expected-power-shelf add --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mypassword --shelf-serial-number DGX-H100-640GB \
    --rack_id 12345678-1234-5678-90ab-cdef01234567

Add an expected power shelf with a BMC IP address and metadata name:
    $ nico-admin-cli expected-power-shelf add --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mypassword --shelf-serial-number DGX-H100-640GB \
    --bmc-ip-address 192.0.2.20 --meta-name shelf-01

")]
pub(crate) struct Args {
    #[clap(
        short = 'a',
        long,
        help = "BMC MAC Address of the expected power shelf"
    )]
    bmc_mac_address: MacAddress,
    #[clap(short = 'u', long, help = "BMC username of the expected power shelf")]
    bmc_username: String,
    #[clap(short = 'p', long, help = "BMC password of the expected power shelf")]
    bmc_password: String,
    #[clap(short = 's', long, help = "Serial number of the expected power shelf")]
    shelf_serial_number: String,

    #[clap(
        long = "meta-name",
        value_name = "META_NAME",
        help = "The name that should be used as part of the Metadata for newly created Power Shelf. If empty, the Power Shelf Id will be used"
    )]
    meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "The description that should be used as part of the Metadata for newly created Power Shelf"
    )]
    meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "A label that will be added as metadata for the newly created Power Shelf. The labels key and value must be separated by a : character. E.g. DATACENTER:XYZ",
        action = clap::ArgAction::Append
    )]
    labels: Option<Vec<String>>,

    #[clap(
        long = "host_name",
        value_name = "HOST_NAME",
        help = "Host name of the power shelf",
        action = clap::ArgAction::Append
    )]
    host_name: Option<String>,

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
        help = "BMC IP address of the power shelf",
        action = clap::ArgAction::Append
    )]
    bmc_ip_address: Option<IpAddr>,

    #[clap(
        long = "bmc-retain-credentials",
        value_name = "BMC_RETAIN_CREDENTIALS",
        help = "When true, site-explorer skips BMC password rotation and stores factory-default credentials in Vault as-is"
    )]
    bmc_retain_credentials: Option<bool>,
}

impl From<Args> for rpc::forge::ExpectedPowerShelf {
    fn from(value: Args) -> Self {
        let labels = parse_rpc_labels(value.labels.unwrap_or_default());
        let metadata = rpc::forge::Metadata {
            name: value.meta_name.unwrap_or_default(),
            description: value.meta_description.unwrap_or_default(),
            labels,
        };
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: value.bmc_mac_address.to_string(),
            bmc_username: value.bmc_username,
            bmc_password: value.bmc_password,
            shelf_serial_number: value.shelf_serial_number,
            bmc_ip_address: value
                .bmc_ip_address
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            rack_id: value.rack_id,
            metadata: Some(metadata),
            bmc_retain_credentials: value.bmc_retain_credentials,
        }
    }
}
