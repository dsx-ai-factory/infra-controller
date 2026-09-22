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

use std::net::SocketAddr;
use std::time;

use carbide_uuid::machine::MachineId;
use eyre::ContextCompat;
use rpc::forge::{Machine, MachinesByIdsRequest};

use crate::api_client;

const MAX_RETRY: usize = 30; // Equal to 30s wait time

pub async fn get_by_id(addrs: &[SocketAddr], machine_id: &MachineId) -> eyre::Result<Machine> {
    let request = MachinesByIdsRequest {
        machine_ids: vec![*machine_id],
        include_history: false,
    };
    let response = api_client::call(addrs, "FindMachinesByIds", |mut client| async move {
        client.find_machines_by_ids(request).await
    })
    .await?;
    response
        .machines
        .into_iter()
        .next()
        .ok_or_else(|| eyre::eyre!("machine {machine_id} was not returned by FindMachinesByIds"))
}

/// Waits for a Machine to reach a certain target state
/// If the Machine does not reach the state within 30s, the function will fail.
pub async fn wait_for_state(
    addrs: &[SocketAddr],
    machine_id: &MachineId,
    target_state: &str,
) -> eyre::Result<()> {
    tracing::info!(
        machine_id = %machine_id,
        target_state,
        "Waiting for Machine state",
    );
    let mut i = 0;
    while i < MAX_RETRY {
        let request = MachinesByIdsRequest {
            machine_ids: vec![*machine_id],
            include_history: false,
        };
        let response = api_client::call(addrs, "FindMachinesByIds", |mut client| async move {
            client.find_machines_by_ids(request).await
        })
        .await?;
        let state = &response
            .machines
            .first()
            .context("FindMachinesByIds returned no machines")?
            .state;
        if state.contains(target_state) {
            break;
        }
        tracing::info!(machine_state = state, "\tCurrent",);
        tokio::time::sleep(time::Duration::from_millis(500)).await;
        i += 1;
    }
    if i == MAX_RETRY {
        eyre::bail!(
            "even after {MAX_RETRY} retries, {machine_id} did not reach state {target_state}"
        );
    }

    Ok(())
}
