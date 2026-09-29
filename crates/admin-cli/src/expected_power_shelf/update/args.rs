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

/// Update an expected power shelf.
///
/// Select the shelf by either BMC MAC address or ID. Supply at least one update field.
/// With Core PATCH, supplied fields replace their stored values and omitted fields remain unchanged.
/// Supplied labels replace the whole label collection. An empty metadata name or description clears that field.
///
/// Supply a BMC username, password, or both. Each omitted credential field keeps its stored value.
/// Core PATCH rejects empty selected credentials. Legacy fallback uses the validation rules on the older server.
///
/// The command first tries Core PATCH, which merges selected fields atomically. It falls back to
/// the legacy update on `Unimplemented` or `PermissionDenied`, or when a MAC lookup returns no ID.
/// The legacy shelf update reads the record, merges selected fields locally, and replaces it.
/// Omitted fields keep their stored values, but concurrent changes can be overwritten on that path.
/// The legacy request still requires authorization. Other PATCH errors and failed legacy updates
/// remain errors.
///
/// https://github.com/dsx-ai-factory/infra-controller/pull/6359
#[derive(Parser, Debug, Clone, Serialize, Deserialize)]
#[clap(verbatim_doc_comment)]
#[command(after_long_help = "\
EXAMPLES:

Correct a power shelf's BMC password while preserving the username:
    $ nico-admin-cli expected-power-shelf update --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-password mynewpassword

Update an expected power shelf's serial number, selecting it by ID:
    $ nico-admin-cli expected-power-shelf update --id 12345678-1234-5678-90ab-cdef01234567 \
    --shelf-serial-number DGX-H100-640GB

")]
#[clap(group(ArgGroup::new("group").required(true).multiple(true).args(&[
"bmc_username",
"bmc_password",
"shelf_serial_number",
"bmc_ip_address",
"bmc_retain_credentials",
"rack_id",
"meta_name",
"meta_description",
"labels",
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
        help = "BMC username of the expected power shelf"
    )]
    bmc_username: Option<String>,
    #[clap(
        short = 'p',
        long,
        group = "group",
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
        help = "Replace the metadata name. An empty value clears it; PATCH preserves it when omitted"
    )]
    meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "Replace the metadata description. An empty value clears it; PATCH preserves it when omitted"
    )]
    meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "Replace all metadata labels with the supplied key or key:value entries. Repeat for each label. Duplicate keys are rejected; label order is not preserved. PATCH preserves omitted labels",
        action = clap::ArgAction::Append
    )]
    labels: Option<Vec<String>>,

    #[clap(
        long = "host_name",
        value_name = "HOST_NAME",
        help = "Unsupported for expected power shelf updates. Omit this option",
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

    pub(super) fn apply_to(
        self,
        shelf: rpc::forge::ExpectedPowerShelf,
    ) -> rpc::forge::ExpectedPowerShelf {
        let metadata =
            if self.meta_name.is_some() || self.meta_description.is_some() || self.labels.is_some()
            {
                let metadata = shelf.metadata.unwrap_or_default();
                Some(rpc::forge::Metadata {
                    name: self.meta_name.unwrap_or(metadata.name),
                    description: self.meta_description.unwrap_or(metadata.description),
                    labels: self
                        .labels
                        .map(crate::metadata::parse_rpc_labels)
                        .unwrap_or(metadata.labels),
                })
            } else {
                shelf.metadata
            };
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: self
                .id
                .map(|id| ::rpc::common::Uuid {
                    value: id.to_string(),
                })
                .or(shelf.expected_power_shelf_id),
            bmc_mac_address: self
                .bmc_mac_address
                .map(|m| m.to_string())
                .unwrap_or(shelf.bmc_mac_address),
            bmc_username: self.bmc_username.unwrap_or(shelf.bmc_username),
            bmc_password: self.bmc_password.unwrap_or(shelf.bmc_password),
            shelf_serial_number: self
                .shelf_serial_number
                .unwrap_or(shelf.shelf_serial_number),
            bmc_ip_address: self
                .bmc_ip_address
                .map(|ip| ip.to_string())
                .unwrap_or(shelf.bmc_ip_address),
            metadata,
            rack_id: self.rack_id.or(shelf.rack_id),
            bmc_retain_credentials: self.bmc_retain_credentials.or(shelf.bmc_retain_credentials),
        }
    }
}

impl From<Args> for rpc::forge::ExpectedPowerShelf {
    fn from(args: Args) -> Self {
        args.apply_to(Self {
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases};
    use rpc::forge::{ExpectedPowerShelf, Label, Metadata};

    use super::*;

    const ID: &str = "12345678-1234-5678-90ab-cdef01234567";

    #[test]
    fn standalone_updates_select_only_the_supplied_field() {
        let base = ExpectedPowerShelf {
            expected_power_shelf_id: Some(rpc::common::Uuid {
                value: ID.to_string(),
            }),
            metadata: Some(Metadata::default()),
            ..Default::default()
        };
        check_cases(
            [
                Case {
                    scenario: "standalone BMC username",
                    input: ["--bmc-username", "new-bmc-user"],
                    expect: Yields((
                        vec!["bmc_username".to_string()],
                        ExpectedPowerShelf {
                            bmc_username: "new-bmc-user".to_string(),
                            ..base.clone()
                        },
                    )),
                },
                Case {
                    scenario: "standalone BMC password",
                    input: ["--bmc-password", "new-bmc-password"],
                    expect: Yields((
                        vec!["bmc_password".to_string()],
                        ExpectedPowerShelf {
                            bmc_password: "new-bmc-password".to_string(),
                            ..base.clone()
                        },
                    )),
                },
                Case {
                    scenario: "standalone BMC IP address",
                    input: ["--bmc-ip-address", "192.0.2.10"],
                    expect: Yields((
                        vec!["bmc_ip_address".to_string()],
                        ExpectedPowerShelf {
                            bmc_ip_address: "192.0.2.10".to_string(),
                            ..base.clone()
                        },
                    )),
                },
                Case {
                    scenario: "explicit false selects credential retention",
                    input: ["--bmc-retain-credentials", "false"],
                    expect: Yields((
                        vec!["bmc_retain_credentials".to_string()],
                        ExpectedPowerShelf {
                            bmc_retain_credentials: Some(false),
                            ..base.clone()
                        },
                    )),
                },
                Case {
                    scenario: "standalone rack uses the existing underscore flag",
                    input: ["--rack_id", "rack-42-us-west"],
                    expect: Yields((
                        vec!["rack_id".to_string()],
                        ExpectedPowerShelf {
                            rack_id: Some("rack-42-us-west".parse().unwrap()),
                            ..base.clone()
                        },
                    )),
                },
                Case {
                    scenario: "empty metadata name selects clearing",
                    input: ["--meta-name", ""],
                    expect: Yields((vec!["metadata.name".to_string()], base.clone())),
                },
                Case {
                    scenario: "empty metadata description selects clearing",
                    input: ["--meta-description", ""],
                    expect: Yields((vec!["metadata.description".to_string()], base.clone())),
                },
                Case {
                    scenario: "standalone labels select the replacement collection",
                    input: ["--label", "team:power"],
                    expect: Yields((
                        vec!["metadata.labels".to_string()],
                        ExpectedPowerShelf {
                            metadata: Some(Metadata {
                                labels: vec![Label {
                                    key: "team".to_string(),
                                    value: Some("power".to_string()),
                                }],
                                ..Default::default()
                            }),
                            ..base
                        },
                    )),
                },
            ],
            |flags| -> Result<_, String> {
                let args = Args::try_parse_from(["update", "--id", ID].into_iter().chain(flags))
                    .map_err(|error| error.to_string())?;
                args.validate().map_err(|error| error.to_string())?;
                let paths = args.update_mask();
                let shelf = ExpectedPowerShelf::from(args);
                Ok((paths, shelf))
            },
        );
    }

    #[test]
    fn updates_require_a_field() {
        assert_eq!(
            Args::try_parse_from(["update", "--id", ID])
                .unwrap_err()
                .kind(),
            ErrorKind::MissingRequiredArgument,
        );
    }
}
