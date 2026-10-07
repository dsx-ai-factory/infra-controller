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

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use clap::{CommandFactory, Parser};

use super::args::*;

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

// Every valid invocation parses, routes to its subcommand variant, and carries
// the expected positional arguments.
#[test]
fn valid_invocations_route_to_their_subcommand() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|cmd| match cmd {
                    Cmd::Inventory => ("inventory", String::new(), String::new()),
                    Cmd::PowerOnSequence(args) => {
                        let request =
                            librms::protos::rack_manager::GetRackPowerOnSequenceRequest::from(args);
                        ("power-on-sequence", request.rack_id, String::new())
                    }
                    Cmd::PowerState(args) => {
                        let request = librms::protos::rack_manager::GetPowerStateRequest::from(args);
                        ("power-state", request.rack_id, request.node_id)
                    }
                    Cmd::FirmwareInventory(args) => {
                        let request =
                            librms::protos::rack_manager::GetNodeFirmwareInventoryRequest::from(args);
                        ("firmware-inventory", request.rack_id, request.node_id)
                    }
                })
                .map_err(drop)
        };
        "inventory takes no args" {
            &["rms", "inventory"][..] => Yields(("inventory", String::new(), String::new())),
        }

        "power-on-sequence carries rack_id" {
            &["rms", "power-on-sequence", "rack-123"][..] => Yields(("power-on-sequence", "rack-123".to_string(), String::new())),
        }

        "power-state carries rack_id and node_id" {
            &["rms", "power-state", "rack-123", "node-123"][..] => Yields((
                "power-state",
                "rack-123".to_string(),
                "node-123".to_string(),
            )),
        }

        "firmware-inventory carries rack_id and node_id" {
            &["rms", "firmware-inventory", "rack-123", "node-123"][..] => Yields((
                "firmware-inventory",
                "rack-123".to_string(),
                "node-123".to_string(),
            )),
        }
    );
}
