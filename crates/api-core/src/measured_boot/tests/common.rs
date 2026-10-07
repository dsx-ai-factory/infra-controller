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

//! tests/common.rs
//!
//! Shared code by measured boot tests.

use std::str::FromStr;

use carbide_uuid::machine::MachineId;
use measured_boot::machine::CandidateMachine;
use model::hardware_info::HardwareInfo;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use sqlx::PgConnection;

pub(super) fn load_topology_json(path: &str) -> HardwareInfo {
    const TEST_DATA_DIR: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/measured_boot/tests/test_data"
    );

    let path = format!("{TEST_DATA_DIR}/{path}");
    let data = std::fs::read(path).unwrap();
    serde_json::from_slice::<HardwareInfo>(&data).unwrap()
}

pub(super) async fn create_test_machine(
    txn: &mut PgConnection,
    machine_id: &str,
    topology: &HardwareInfo,
) -> eyre::Result<CandidateMachine> {
    let machine_id = MachineId::from_str(machine_id)?;
    db::machine::create(
        txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    db::machine_topology::create_or_update(txn, &machine_id, topology).await?;
    let machine = db::measured_boot::machine::from_id(txn, machine_id).await?;
    assert_eq!(machine_id, machine.machine_id);
    Ok(machine)
}
