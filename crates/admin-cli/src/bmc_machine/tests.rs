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

// The intent of the tests.rs file is to test the integrity of the
// command, including things like basic structure parsing, enum
// translations, and any external input validators that are
// configured. Specific "categories" are:
//
// Command Structure - Baseline debug_assert() of the entire command.
// Argument Parsing  - Ensure required/optional arg combinations parse correctly.
// Enum Conversions  - Test From implementations for proto <-> non-proto mapping.
// ValueEnum Parsing - Test string parsing for types deriving claps ValueEnum.

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use clap::{CommandFactory, Parser};

use super::common::{AdminPowerControlAction, ResetTypeArg};
use super::*;
use crate::test_support::{parse_leaf, parse_with_leaf_matches, raw_value};

// A valid SwitchId and PowerShelfId for exercising the bmc-reset targets.
const TEST_SWITCH_ID: &str = "sw100nsmnq69j4ntqlj162fnnbvg747gfqbicaa6tqgq6spocirfle7rom0";
const TEST_POWER_SHELF_ID: &str = "ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";

// Define a basic/working MachineId for testing.
const TEST_MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

// verify_cmd_structure runs a baseline clap debug_assert()
// to do basic command configuration checking and validation,
// ensuring things like unique argument definitions, group
// configurations, argument references, etc. Things that would
// otherwise be missed until runtime.
#[test]
fn verify_cmd_structure() {
    Cmd::command().debug_assert();
}

/////////////////////////////////////////////////////////////////////////////
// Argument Parsing
//
// This section contains tests specific to argument parsing,
// including testing required arguments, as well as optional
// flag-specific checking.

// parse_bmc_reset routes to the BmcReset variant; --use-ipmitool toggles the
// flag (default off) and the --machine target parses as a typed MachineId.
#[test]
fn parse_bmc_reset() {
    scenarios!(
        run = |argv| {
            parse_leaf::<Cmd>(argv, &["bmc-reset"])
                .map(|matches| {
                    (
                        raw_value(&matches, "machine").expect("machine is required"),
                        matches.get_flag("use_ipmitool"),
                    )
                })
                .map_err(drop)
        };
        "bmc-reset with required args, ipmitool off" {
            &["bmc-machine", "bmc-reset", "--machine", TEST_MACHINE_ID][..] => Yields((TEST_MACHINE_ID.to_string(), false)),
        }

        "bmc-reset with --use-ipmitool" {
            &[
                "bmc-machine",
                "bmc-reset",
                "--machine",
                TEST_MACHINE_ID,
                "--use-ipmitool",
            ][..] => Yields((TEST_MACHINE_ID.to_string(), true)),
        }
    );
}

// Each of the three mutually exclusive targets converts to the matching
// DeviceId variant, and --reset-type maps onto the request's ResetType.
#[test]
fn bmc_reset_targets_convert_to_device_id() {
    use carbide_uuid::device::DeviceId;
    use rpc::forge::AdminBmcResetRequest;
    use rpc::forge::admin_bmc_reset_request::ResetType;

    let request = |argv: &[&str]| -> AdminBmcResetRequest {
        let (cmd, _) =
            parse_with_leaf_matches::<Cmd>(argv, &["bmc-reset"]).expect("bmc-reset should parse");
        match cmd {
            Cmd::BmcReset(args) => AdminBmcResetRequest::from(args),
            other => panic!("expected BmcReset, got {other:?}"),
        }
    };

    let machine = request(&["bmc-machine", "bmc-reset", "--machine", TEST_MACHINE_ID]);
    assert!(matches!(machine.device_id, Some(DeviceId::Machine(_))));
    assert_eq!(machine.reset_type, ResetType::Unspecified as i32);

    let switch = request(&[
        "bmc-machine",
        "bmc-reset",
        "--switch",
        TEST_SWITCH_ID,
        "--reset-type",
        "force",
    ]);
    assert!(matches!(switch.device_id, Some(DeviceId::Switch(_))));
    assert_eq!(switch.reset_type, ResetType::ForceRestart as i32);

    let power_shelf = request(&[
        "bmc-machine",
        "bmc-reset",
        "--power-shelf",
        TEST_POWER_SHELF_ID,
        "--reset-type",
        "graceful",
    ]);
    assert!(matches!(
        power_shelf.device_id,
        Some(DeviceId::PowerShelf(_))
    ));
    assert_eq!(power_shelf.reset_type, ResetType::GracefulRestart as i32);
}

// The target flags form a required, mutually exclusive group: exactly one of
// --machine/--switch/--power-shelf must be given.
#[test]
fn bmc_reset_requires_exactly_one_target() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "no target is rejected" {
            &["bmc-machine", "bmc-reset"][..] => Fails,
        }

        "two targets are rejected" {
            &[
                "bmc-machine",
                "bmc-reset",
                "--machine",
                TEST_MACHINE_ID,
                "--switch",
                TEST_SWITCH_ID,
            ][..] => Fails,
        }

        "one target parses" {
            &["bmc-machine", "bmc-reset", "--switch", TEST_SWITCH_ID][..] => Yields(()),
        }
    );
}

// parse_admin_power_control routes to the AdminPowerControl variant; the
// --machine value is captured and --action on maps to the On action.
#[test]
fn parse_admin_power_control() {
    scenarios!(
        run = |argv| {
            parse_leaf::<Cmd>(argv, &["admin-power-control"])
                .map(|matches| {
                    (
                        raw_value(&matches, "machine").expect("machine is required"),
                        matches!(
                            matches.get_one::<AdminPowerControlAction>("action"),
                            Some(AdminPowerControlAction::On)
                        ),
                    )
                })
                .map_err(drop)
        };
        "admin-power-control --action on" {
            &[
                "bmc-machine",
                "admin-power-control",
                "--machine",
                "machine-123",
                "--action",
                "on",
            ][..] => Yields(("machine-123".to_string(), true)),
        }
    );
}

// parse_lockdown routes to the Lockdown variant; --enable and --disable are
// mutually exclusive flags, each setting exactly its own bool.
#[test]
fn parse_lockdown() {
    scenarios!(
        run = |argv| {
            parse_leaf::<Cmd>(argv, &["lockdown"])
                .map(|matches| {
                    (matches.get_flag("enable"), matches.get_flag("disable"))
                })
                .map_err(drop)
        };
        "lockdown --enable" {
            &[
                "bmc-machine",
                "lockdown",
                "--machine",
                TEST_MACHINE_ID,
                "--enable",
            ][..] => Yields((true, false)),
        }

        "lockdown --disable" {
            &[
                "bmc-machine",
                "lockdown",
                "--machine",
                TEST_MACHINE_ID,
                "--disable",
            ][..] => Yields((false, true)),
        }
    );
}

// parse_create_bmc_user routes to the CreateBmcUser variant, capturing the
// username, password, and optional IP address.
#[test]
fn parse_create_bmc_user() {
    scenarios!(
        run = |argv| {
            parse_leaf::<Cmd>(argv, &["create-bmc-user"])
                .map(|matches| {
                    (
                        raw_value(&matches, "username").expect("username is required"),
                        raw_value(&matches, "password").expect("password is required"),
                        raw_value(&matches, "ip_address"),
                    )
                })
                .map_err(drop)
        };
        "create-bmc-user with username, password, and ip" {
            &[
                "bmc-machine",
                "create-bmc-user",
                "--username",
                "admin",
                "--password",
                "secret123",
                "--ip-address",
                "192.168.1.100",
            ][..] => Yields((
                "admin".to_string(),
                "secret123".to_string(),
                Some("192.168.1.100".to_string()),
            )),
        }
    );
}

// Every malformed lockdown invocation is rejected at parse time -- neither
// --enable nor --disable, or both at once (a conflict).
#[test]
fn invalid_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "lockdown without --enable or --disable" {
            &["bmc-machine", "lockdown", "--machine", TEST_MACHINE_ID][..] => Fails,
        }

        "lockdown with both --enable and --disable" {
            &[
                "bmc-machine",
                "lockdown",
                "--machine",
                TEST_MACHINE_ID,
                "--enable",
                "--disable",
            ][..] => Fails,
        }
    );
}

/////////////////////////////////////////////////////////////////////////////
// Enum Conversions
//
// This section is for testing the proto <-> non-proto enum
// From implementations that exist, ensuring enums translate
// from -> into their expected variants.

// admin_power_control_action_to_proto ensures
// AdminPowerControlAction converts to protobuf.
#[test]
fn admin_power_control_action_to_proto() {
    use rpc::forge::admin_power_control_request::SystemPowerControl;

    assert!(matches!(
        SystemPowerControl::from(AdminPowerControlAction::On),
        SystemPowerControl::On
    ));
    assert!(matches!(
        SystemPowerControl::from(AdminPowerControlAction::GracefulShutdown),
        SystemPowerControl::GracefulShutdown
    ));
    assert!(matches!(
        SystemPowerControl::from(AdminPowerControlAction::ForceOff),
        SystemPowerControl::ForceOff
    ));
    assert!(matches!(
        SystemPowerControl::from(AdminPowerControlAction::GracefulRestart),
        SystemPowerControl::GracefulRestart
    ));
    assert!(matches!(
        SystemPowerControl::from(AdminPowerControlAction::ForceRestart),
        SystemPowerControl::ForceRestart
    ));
    assert!(matches!(
        SystemPowerControl::from(AdminPowerControlAction::ACPowercycle),
        SystemPowerControl::AcPowercycle
    ));
}

/////////////////////////////////////////////////////////////////////////////
// ValueEnum Parsing
//
// These tests are for testing argument values which derive
// ValueEnum, ensuring the string representations of said
// values correctly convert back into their expected variant,
// or fail otherwise.

// admin_power_control_action_value_enum ensures AdminPowerControlAction
// parses from strings.
#[test]
fn admin_power_control_action_value_enum() {
    use clap::ValueEnum;

    assert!(matches!(
        AdminPowerControlAction::from_str("on", false),
        Ok(AdminPowerControlAction::On)
    ));
    assert!(matches!(
        AdminPowerControlAction::from_str("graceful-shutdown", false),
        Ok(AdminPowerControlAction::GracefulShutdown)
    ));
    assert!(matches!(
        AdminPowerControlAction::from_str("force-off", false),
        Ok(AdminPowerControlAction::ForceOff)
    ));
    assert!(matches!(
        AdminPowerControlAction::from_str("graceful-restart", false),
        Ok(AdminPowerControlAction::GracefulRestart)
    ));
    assert!(matches!(
        AdminPowerControlAction::from_str("force-restart", false),
        Ok(AdminPowerControlAction::ForceRestart)
    ));
    assert!(matches!(
        AdminPowerControlAction::from_str("ac-powercycle", false),
        Ok(AdminPowerControlAction::ACPowercycle)
    ));
    assert!(AdminPowerControlAction::from_str("invalid", false).is_err());
}

// reset_type_arg_to_proto ensures ResetTypeArg converts to the request's
// ResetType (graceful -> GracefulRestart, force -> ForceRestart).
#[test]
fn reset_type_arg_to_proto() {
    use rpc::forge::admin_bmc_reset_request::ResetType;

    assert!(matches!(
        ResetType::from(ResetTypeArg::Graceful),
        ResetType::GracefulRestart
    ));
    assert!(matches!(
        ResetType::from(ResetTypeArg::Force),
        ResetType::ForceRestart
    ));
}

// reset_type_arg_value_enum ensures ResetTypeArg parses from its kebab-case
// string forms and rejects anything else.
#[test]
fn reset_type_arg_value_enum() {
    use clap::ValueEnum;

    assert!(matches!(
        ResetTypeArg::from_str("graceful", false),
        Ok(ResetTypeArg::Graceful)
    ));
    assert!(matches!(
        ResetTypeArg::from_str("force", false),
        Ok(ResetTypeArg::Force)
    ));
    assert!(ResetTypeArg::from_str("invalid", false).is_err());
}
