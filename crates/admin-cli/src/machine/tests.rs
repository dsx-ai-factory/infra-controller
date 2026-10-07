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

use carbide_uuid::machine::MachineId;
use clap::CommandFactory;

use super::*;
use crate::test_support::parse_leaf;

const SAMPLE_MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

#[test]
fn verify_cmd_structure() {
    Cmd::command().debug_assert();
}

#[test]
fn parse_health_history_command() {
    let matches = parse_leaf::<Cmd>(
        &["machine", "health-history", SAMPLE_MACHINE_ID],
        &["health-history"],
    )
    .expect("health-history should parse");
    let machine_id = matches
        .get_one::<MachineId>("machine_id")
        .expect("machine ID is required");
    assert_eq!(machine_id, &SAMPLE_MACHINE_ID.parse::<MachineId>().unwrap());
}
