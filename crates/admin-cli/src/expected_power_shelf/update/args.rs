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

use crate::errors::CarbideCliError;

#[derive(Parser, Debug, Serialize, Deserialize)]
#[command(after_long_help = "\
EXAMPLES:

Update an expected power shelf's BMC credentials, selecting it by MAC address:
    $ nico-admin-cli expected-power-shelf update --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin --bmc-password mynewpassword

Update an expected power shelf's serial number, selecting it by ID:
    $ nico-admin-cli expected-power-shelf update --id 12345678-1234-5678-90ab-cdef01234567 \
    --shelf-serial-number DGX-H100-640GB

")]
#[clap(group(ArgGroup::new("group").required(true).multiple(true).args(&[
"bmc_username",
"bmc_password",
"shelf_serial_number",
])))]
pub(crate) struct Args {
    #[clap(
        short = 'a',
        long,
        help = "BMC MAC Address of the expected power shelf"
    )]
    bmc_mac_address: Option<MacAddress>,

    #[clap(long = "id", help = "ID (UUID) of the expected power shelf to update.")]
    #[serde(skip)]
    id: Option<Uuid>,
    #[clap(
        short = 'u',
        long,
        group = "group",
        requires("bmc_password"),
        help = "BMC username of the expected power shelf"
    )]
    bmc_username: Option<String>,
    #[clap(
        short = 'p',
        long,
        group = "group",
        requires("bmc_username"),
        help = "BMC password of the expected power shelf"
    )]
    bmc_password: Option<String>,
    #[clap(
        short = 's',
        long,
        group = "group",
        help = "Chassis serial number of the expected power shelf"
    )]
    shelf_serial_number: Option<String>,

    #[clap(
        long = "meta-name",
        value_name = "META_NAME",
        help = "The name that should be used as part of the Metadata for newly created Power Shelves. If empty, the Power Shelf Id will be used"
    )]
    meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "The description that should be used as part of the Metadata for newly created Power Shelves"
    )]
    meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "A label that will be added as metadata for the newly created Machine. The labels key and value must be separated by a : character",
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
        help = "Rack ID for this power shelf",
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

impl Args {
    pub(super) fn validate(&self) -> Result<(), clap::Error> {
        let error = |kind, message: &str| {
            Self::command()
                .bin_name("nico-admin-cli expected-power-shelf update")
                .error(kind, message)
        };
        match (&self.bmc_mac_address, &self.id) {
            (Some(_), Some(_)) => {
                return Err(error(
                    ErrorKind::ArgumentConflict,
                    "cannot specify both --bmc-mac-address and --id; provide only one",
                ));
            }
            (None, None) => {
                return Err(error(
                    ErrorKind::MissingRequiredArgument,
                    "must specify either --bmc-mac-address or --id",
                ));
            }
            _ => {}
        }
        if self.host_name.is_some() {
            return Err(error(
                ErrorKind::ValueValidation,
                "--host_name is not supported for expected power shelf updates; remove it from the command",
            ));
        }
        Ok(())
    }

    pub(super) fn update_mask(&self) -> Vec<String> {
        [
            (self.bmc_username.is_some(), "bmc_username"),
            (self.bmc_password.is_some(), "bmc_password"),
            (self.shelf_serial_number.is_some(), "shelf_serial_number"),
            (self.bmc_ip_address.is_some(), "bmc_ip_address"),
            (
                self.bmc_retain_credentials.is_some(),
                "bmc_retain_credentials",
            ),
            (self.rack_id.is_some(), "rack_id"),
            (self.meta_name.is_some(), "metadata.name"),
            (self.meta_description.is_some(), "metadata.description"),
            (self.labels.is_some(), "metadata.labels"),
        ]
        .into_iter()
        .filter(|(provided, _)| *provided)
        .map(|(_, path)| path.to_string())
        .collect()
    }
}

impl TryFrom<Args> for rpc::forge::ExpectedPowerShelf {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> Result<Self, Self::Error> {
        if args.bmc_username.is_none()
            && args.bmc_password.is_none()
            && args.shelf_serial_number.is_none()
        {
            return Err(CarbideCliError::GenericError(
                "One of the following options must be specified: bmc-user-name and bmc-password or shelf-serial-number".to_string(),
            ));
        }
        Ok(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: args.id.map(|id| ::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: args
                .bmc_mac_address
                .map(|m| m.to_string())
                .unwrap_or_default(),
            bmc_username: args.bmc_username.unwrap_or_default(),
            bmc_password: args.bmc_password.unwrap_or_default(),
            shelf_serial_number: args.shelf_serial_number.unwrap_or_default(),
            bmc_ip_address: args
                .bmc_ip_address
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            metadata: Some(rpc::forge::Metadata {
                name: args.meta_name.unwrap_or_default(),
                description: args.meta_description.unwrap_or_default(),
                labels: crate::metadata::parse_rpc_labels(args.labels.unwrap_or_default()),
            }),
            rack_id: args.rack_id,
            bmc_retain_credentials: args.bmc_retain_credentials,
        })
    }
}
