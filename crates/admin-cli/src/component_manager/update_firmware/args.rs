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

use std::path::PathBuf;

use clap::{Args as ClapArgs, Parser, Subcommand};

use crate::component_manager::common::{
    ComputeTrayComponentArg, ComputeTraySelection, ComputeTrayTargetArgs, NvSwitchComponentArg,
    PowerShelfComponentArg, PowerShelfTargetArgs, RackTargetArgs, SwitchTargetArgs,
};
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Queue firmware on NVLink switches from a target version:
    $ nico-admin-cli component-manager update-firmware switch \
    --switch-id 12345678-1234-5678-90ab-cdef01234567 --target-version fw-1.2.3

Update only specific switch components, forcing the update:
    $ nico-admin-cli component-manager update-firmware switch \
    --switch-id 12345678-1234-5678-90ab-cdef01234567 --component bmc,bios --force-update \
    --target-version fw-1.2.3

Queue firmware on compute trays from an RMS SOT JSON file:
    $ nico-admin-cli component-manager update-firmware compute-tray \
    --machine-id 12345678-1234-5678-90ab-cdef01234567 --sot-json-file ./sot.json \
    --access-token mytoken

Queue firmware on a compute tray by BMC MAC (targets the tray before ingestion; \
this dispatches directly to RMS with NOAUTH artifact downloads, so --access-token \
is not accepted here):
    $ nico-admin-cli component-manager update-firmware compute-tray \
    --mac-address 00:11:22:33:44:55 --sot-json-file ./sot.json

Queue firmware on power shelves:
    $ nico-admin-cli component-manager update-firmware power-shelf \
    --power-shelf-id 12345678-1234-5678-90ab-cdef01234567 --target-version fw-1.2.3

Queue firmware on all eligible devices in a rack:
    $ nico-admin-cli component-manager update-firmware rack \
    --rack-id 12345678-1234-5678-90ab-cdef01234567 --target-version fw-1.2.3

")]
pub(crate) struct Args {
    #[clap(subcommand)]
    target: Target,
}

#[derive(Subcommand, Debug)]
enum Target {
    #[clap(about = "Queue firmware on NVLink switches")]
    Switch(SwitchArgs),

    #[clap(about = "Queue firmware on power shelves")]
    PowerShelf(PowerShelfArgs),

    #[clap(about = "Queue firmware on compute trays")]
    ComputeTray(ComputeTrayArgs),

    #[clap(about = "Queue firmware on all eligible devices in racks")]
    Rack(RackArgs),
}

#[derive(ClapArgs, Debug)]
struct SwitchArgs {
    #[clap(flatten)]
    ids: SwitchTargetArgs,

    #[clap(flatten)]
    firmware_source: FirmwareSourceArgs,

    #[clap(long = "force-update", help = "Force firmware update when supported")]
    force_update: bool,

    #[clap(
        long = "component",
        value_enum,
        value_delimiter = ',',
        help = "NVLink switch components to update; omit to update all supported components"
    )]
    components: Vec<NvSwitchComponentArg>,

    #[clap(
        long = "bypass-state-controller",
        help = "Bypass the state controller and dispatch directly to the component backend"
    )]
    bypass_state_controller: bool,
}

#[derive(ClapArgs, Debug)]
struct PowerShelfArgs {
    #[clap(flatten)]
    ids: PowerShelfTargetArgs,

    #[clap(long = "target-version", help = "Firmware target version")]
    target_version: String,

    #[clap(long = "force-update", help = "Force firmware update when supported")]
    force_update: bool,

    #[clap(
        long = "component",
        value_enum,
        value_delimiter = ',',
        help = "Power shelf components to update; omit to update all supported components"
    )]
    components: Vec<PowerShelfComponentArg>,

    #[clap(
        long = "bypass-state-controller",
        help = "Bypass the state controller and dispatch directly to the component backend"
    )]
    bypass_state_controller: bool,
}

#[derive(ClapArgs, Debug)]
struct ComputeTrayArgs {
    #[clap(flatten)]
    ids: ComputeTrayTargetArgs,

    #[clap(flatten)]
    firmware_source: FirmwareSourceArgs,

    #[clap(long = "force-update", help = "Force firmware update when supported")]
    force_update: bool,

    #[clap(
        long = "component",
        value_enum,
        value_delimiter = ',',
        help = "Compute tray components to update; omit to update all supported components"
    )]
    components: Vec<ComputeTrayComponentArg>,

    #[clap(
        long = "bypass-state-controller",
        help = "Bypass the state controller and dispatch directly to the component backend"
    )]
    bypass_state_controller: bool,
}

#[derive(ClapArgs, Debug)]
struct RackArgs {
    #[clap(flatten)]
    ids: RackTargetArgs,

    #[clap(flatten)]
    firmware_source: FirmwareSourceArgs,

    #[clap(long = "force-update", help = "Force firmware update when supported")]
    force_update: bool,
}

#[derive(ClapArgs, Debug)]
struct FirmwareSourceArgs {
    #[clap(
        long = "target-version",
        help = "Firmware target version for legacy direct-update paths"
    )]
    target_version: Option<String>,

    #[clap(
        long = "sot-json-file",
        value_name = "PATH",
        help = "SOT JSON file for RMS ApplyFirmwareObject"
    )]
    sot_json_file: Option<PathBuf>,

    #[clap(
        long = "access-token",
        help = "Artifact access token for RMS SOT JSON downloads; omit or pass empty for NOAUTH"
    )]
    access_token: Option<String>,
}

fn resolve_firmware_source(
    source: FirmwareSourceArgs,
) -> CarbideCliResult<(String, Option<String>)> {
    match (
        source.target_version,
        source.sot_json_file,
        source.access_token,
    ) {
        (Some(_), Some(_), _) => Err(CarbideCliError::ChooseOneError(
            "--target-version",
            "--sot-json-file",
        )),
        (None, None, _) => Err(CarbideCliError::RequireOneError(
            "--target-version",
            "--sot-json-file",
        )),
        (Some(_), None, Some(_)) => Err(CarbideCliError::GenericError(
            "--access-token requires --sot-json-file".to_string(),
        )),
        (Some(target_version), None, None) => {
            if target_version.trim().is_empty() {
                Err(CarbideCliError::GenericError(
                    "--target-version must not be empty".to_string(),
                ))
            } else {
                Ok((target_version, None))
            }
        }
        (None, Some(sot_json_file), access_token) => {
            let token = access_token.filter(|token| !token.trim().is_empty());

            let config_json = std::fs::read_to_string(sot_json_file)?;
            serde_json::from_str::<serde_json::Value>(&config_json)?;
            Ok((config_json, token))
        }
    }
}

impl TryFrom<Args> for rpc::forge::UpdateComponentFirmwareRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        match args.target {
            Target::Switch(target) => {
                let (target_version, access_token) =
                    resolve_firmware_source(target.firmware_source)?;
                Ok(Self {
                    target_version,
                    access_token,
                    force_update: target.force_update,
                    bypass_state_controller: target.bypass_state_controller,
                    target: Some(
                        rpc::forge::update_component_firmware_request::Target::Switches(
                            rpc::forge::UpdateSwitchFirmwareTarget {
                                switch_ids: Some(target.ids.into()),
                                components: target
                                    .components
                                    .into_iter()
                                    .map(|component| {
                                        rpc::forge::NvSwitchComponent::from(component) as i32
                                    })
                                    .collect(),
                            },
                        ),
                    ),
                })
            }
            Target::PowerShelf(target) => Ok(Self {
                target_version: target.target_version,
                access_token: None,
                force_update: target.force_update,
                bypass_state_controller: target.bypass_state_controller,
                target: Some(
                    rpc::forge::update_component_firmware_request::Target::PowerShelves(
                        rpc::forge::UpdatePowerShelfFirmwareTarget {
                            power_shelf_ids: Some(target.ids.into()),
                            components: target
                                .components
                                .into_iter()
                                .map(|component| {
                                    rpc::forge::PowerShelfComponent::from(component) as i32
                                })
                                .collect(),
                        },
                    ),
                ),
            }),
            Target::ComputeTray(target) => {
                let (target_version, access_token) =
                    resolve_firmware_source(target.firmware_source)?;
                Ok(Self {
                    target_version,
                    access_token,
                    force_update: target.force_update,
                    bypass_state_controller: target.bypass_state_controller,
                    target: Some(
                        rpc::forge::update_component_firmware_request::Target::ComputeTrays({
                            let (machine_ids, bmc_macs) = match target.ids.into_selection() {
                                ComputeTraySelection::MachineIds(list) => (Some(list), None),
                                ComputeTraySelection::Macs(macs) => (None, Some(macs)),
                            };
                            rpc::forge::UpdateComputeTrayFirmwareTarget {
                                machine_ids,
                                bmc_macs,
                                components: target
                                    .components
                                    .into_iter()
                                    .map(|component| {
                                        rpc::forge::ComputeTrayComponent::from(component) as i32
                                    })
                                    .collect(),
                            }
                        }),
                    ),
                })
            }
            Target::Rack(target) => {
                let (target_version, access_token) =
                    resolve_firmware_source(target.firmware_source)?;
                Ok(Self {
                    target_version,
                    access_token,
                    force_update: target.force_update,
                    bypass_state_controller: false,
                    target: Some(
                        rpc::forge::update_component_firmware_request::Target::Racks(
                            rpc::forge::UpdateFirmwareObjectTarget {
                                rack_ids: Some(target.ids.into()),
                            },
                        ),
                    ),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    fn temp_sot_file(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bmm-sot-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        ));
        std::fs::write(&path, contents).expect("write test SOT JSON");
        path
    }

    // A firmware source resolves to its (target_version, access_token) pair:
    // a legacy --target-version passes through verbatim with no token, while a
    // SOT JSON file resolves to the file's contents paired with the token --
    // an empty token collapsing to None.
    #[test]
    fn firmware_source_resolves_target_version_and_token() {
        let sot_token = temp_sot_file(r#"{"Id":"fw-object"}"#);
        let sot_no_token = temp_sot_file(r#"{"Id":"fw-object"}"#);
        let sot_empty_token = temp_sot_file(r#"{"Id":"fw-object"}"#);

        scenarios!(
            run = |source| resolve_firmware_source(source).map_err(drop);
            "legacy --target-version passes through with no token" {
                FirmwareSourceArgs {
                    target_version: Some("fw-1.0".to_string()),
                    sot_json_file: None,
                    access_token: None,
                } => Yields(("fw-1.0".to_string(), None)),
            }

            "SOT JSON file resolves to its contents and the access token" {
                FirmwareSourceArgs {
                    target_version: None,
                    sot_json_file: Some(sot_token.clone()),
                    access_token: Some("token".to_string()),
                } => Yields((
                    r#"{"Id":"fw-object"}"#.to_string(),
                    Some("token".to_string()),
                )),
            }

            "SOT JSON file resolves without an access token" {
                FirmwareSourceArgs {
                    target_version: None,
                    sot_json_file: Some(sot_no_token.clone()),
                    access_token: None,
                } => Yields((r#"{"Id":"fw-object"}"#.to_string(), None)),
            }

            "an empty access token collapses to None" {
                FirmwareSourceArgs {
                    target_version: None,
                    sot_json_file: Some(sot_empty_token.clone()),
                    access_token: Some(String::new()),
                } => Yields((r#"{"Id":"fw-object"}"#.to_string(), None)),
            }
        );

        let _ = std::fs::remove_file(sot_token);
        let _ = std::fs::remove_file(sot_no_token);
        let _ = std::fs::remove_file(sot_empty_token);
    }

    // A firmware source is rejected when its parts are incoherent: an access
    // token without a SOT JSON file has nothing to authenticate, and a SOT JSON
    // file whose contents aren't valid JSON can't be parsed. (CarbideCliError is
    // not PartialEq, so these assert only that resolution fails.)
    #[test]
    fn firmware_source_rejects_incoherent_inputs() {
        let invalid_json = temp_sot_file("not-json");

        scenarios!(
            run = |source| resolve_firmware_source(source).map_err(drop);
            "both firmware sources are rejected" {
                FirmwareSourceArgs {
                    target_version: Some("fw-1.0".to_string()),
                    sot_json_file: Some(invalid_json.clone()),
                    access_token: None,
                } => Fails,
            }

            "a missing firmware source is rejected" {
                FirmwareSourceArgs {
                    target_version: None,
                    sot_json_file: None,
                    access_token: None,
                } => Fails,
            }

            "an empty target version is rejected" {
                FirmwareSourceArgs {
                    target_version: Some(" ".to_string()),
                    sot_json_file: None,
                    access_token: None,
                } => Fails,
            }

            "access token without a SOT JSON file" {
                FirmwareSourceArgs {
                    target_version: Some("fw-1.0".to_string()),
                    sot_json_file: None,
                    access_token: Some("token".to_string()),
                } => Fails,
            }

            "SOT JSON file whose contents are not valid JSON" {
                FirmwareSourceArgs {
                    target_version: None,
                    sot_json_file: Some(invalid_json.clone()),
                    access_token: Some("token".to_string()),
                } => Fails,
            }
        );

        let _ = std::fs::remove_file(invalid_json);
    }

    #[test]
    fn update_firmware_commands_build_requests_for_every_target() {
        const CONFIG_JSON: &str = r#"{"Id":"fw-object","Version":"1.2.3"}"#;
        const MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
        const MAC_ADDRESS: &str = "00:11:22:33:44:55";
        const POWER_SHELF_ID: &str = "ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";
        const RACK_ID: &str = "rack-test";
        const SWITCH_ID: &str = "sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";

        let sot_json = temp_sot_file(CONFIG_JSON);
        let sot_json = sot_json.to_str().expect("temporary path is UTF-8");

        let switch_request = rpc::forge::UpdateComponentFirmwareRequest::try_from(
            Args::try_parse_from([
                "update-firmware",
                "switch",
                "--switch-id",
                SWITCH_ID,
                "--sot-json-file",
                sot_json,
                "--access-token",
                "token",
                "--component",
                "bmc,nvos",
                "--force-update",
                "--bypass-state-controller",
            ])
            .expect("switch command should parse"),
        )
        .expect("switch command should build a request");

        assert_eq!(switch_request.target_version, CONFIG_JSON);
        assert_eq!(switch_request.access_token.as_deref(), Some("token"));
        assert!(switch_request.force_update);
        assert!(switch_request.bypass_state_controller);
        let Some(rpc::forge::update_component_firmware_request::Target::Switches(target)) =
            switch_request.target
        else {
            panic!("switch command should build a switch target");
        };

        let switch_ids = target.switch_ids.expect("switch IDs");

        assert_eq!(switch_ids.ids.len(), 1);
        assert_eq!(switch_ids.ids[0].to_string(), SWITCH_ID);

        assert_eq!(
            target.components,
            [
                rpc::forge::NvSwitchComponent::Bmc as i32,
                rpc::forge::NvSwitchComponent::Nvos as i32,
            ]
        );

        let compute_request = rpc::forge::UpdateComponentFirmwareRequest::try_from(
            Args::try_parse_from([
                "update-firmware",
                "compute-tray",
                "--machine-id",
                MACHINE_ID,
                "--target-version",
                "fw-1.2.3",
                "--component",
                "bmc,bios",
                "--force-update",
                "--bypass-state-controller",
            ])
            .expect("compute-tray command should parse"),
        )
        .expect("compute-tray command should build a request");

        assert_eq!(compute_request.target_version, "fw-1.2.3");
        assert_eq!(compute_request.access_token, None);
        assert!(compute_request.force_update);
        assert!(compute_request.bypass_state_controller);
        let Some(rpc::forge::update_component_firmware_request::Target::ComputeTrays(target)) =
            compute_request.target
        else {
            panic!("compute-tray command should build a compute-tray target");
        };

        let Some(machine_ids) = target.machine_ids else {
            panic!("compute-tray command should build a machine-id target");
        };
        assert!(
            target.bmc_macs.is_none(),
            "machine-id target must not also set bmc_macs",
        );

        assert_eq!(machine_ids.machine_ids.len(), 1);
        assert_eq!(machine_ids.machine_ids[0].to_string(), MACHINE_ID);

        assert_eq!(
            target.components,
            [
                rpc::forge::ComputeTrayComponent::Bmc as i32,
                rpc::forge::ComputeTrayComponent::Bios as i32,
            ]
        );

        let compute_mac_request = rpc::forge::UpdateComponentFirmwareRequest::try_from(
            Args::try_parse_from([
                "update-firmware",
                "compute-tray",
                "--mac-address",
                MAC_ADDRESS,
                "--target-version",
                "fw-1.2.3",
            ])
            .expect("compute-tray MAC command should parse"),
        )
        .expect("compute-tray MAC command should build a request");

        let Some(rpc::forge::update_component_firmware_request::Target::ComputeTrays(target)) =
            compute_mac_request.target
        else {
            panic!("compute-tray MAC command should build a compute-tray target");
        };

        let Some(bmc_macs) = target.bmc_macs else {
            panic!("compute-tray MAC command should build a bmc-macs target");
        };
        assert!(
            target.machine_ids.is_none(),
            "MAC target must not also set machine_ids",
        );

        assert_eq!(bmc_macs.mac_addresses, [MAC_ADDRESS]);

        let power_shelf_request = rpc::forge::UpdateComponentFirmwareRequest::try_from(
            Args::try_parse_from([
                "update-firmware",
                "power-shelf",
                "--power-shelf-id",
                POWER_SHELF_ID,
                "--target-version",
                "fw-1.2.3",
                "--component",
                "pmc,psu",
                "--force-update",
                "--bypass-state-controller",
            ])
            .expect("power-shelf command should parse"),
        )
        .expect("power-shelf command should build a request");

        assert_eq!(power_shelf_request.target_version, "fw-1.2.3");
        assert_eq!(power_shelf_request.access_token, None);
        assert!(power_shelf_request.force_update);
        assert!(power_shelf_request.bypass_state_controller);
        let Some(rpc::forge::update_component_firmware_request::Target::PowerShelves(target)) =
            power_shelf_request.target
        else {
            panic!("power-shelf command should build a power-shelf target");
        };

        let power_shelf_ids = target.power_shelf_ids.expect("power shelf IDs");

        assert_eq!(power_shelf_ids.ids.len(), 1);
        assert_eq!(power_shelf_ids.ids[0].to_string(), POWER_SHELF_ID);

        assert_eq!(
            target.components,
            [
                rpc::forge::PowerShelfComponent::Pmc as i32,
                rpc::forge::PowerShelfComponent::Psu as i32,
            ]
        );

        let rack_request = rpc::forge::UpdateComponentFirmwareRequest::try_from(
            Args::try_parse_from([
                "update-firmware",
                "rack",
                "--rack-id",
                RACK_ID,
                "--sot-json-file",
                sot_json,
                "--access-token",
                "token",
                "--force-update",
            ])
            .expect("rack command should parse"),
        )
        .expect("rack command should build a request");

        assert_eq!(rack_request.target_version, CONFIG_JSON);
        assert_eq!(rack_request.access_token.as_deref(), Some("token"));
        assert!(rack_request.force_update);
        assert!(!rack_request.bypass_state_controller);
        let Some(rpc::forge::update_component_firmware_request::Target::Racks(target)) =
            rack_request.target
        else {
            panic!("rack command should build a rack target");
        };

        let rack_ids = target.rack_ids.expect("rack IDs");

        assert_eq!(rack_ids.rack_ids.len(), 1);
        assert_eq!(rack_ids.rack_ids[0].to_string(), RACK_ID);

        let _ = std::fs::remove_file(sot_json);
    }
}
