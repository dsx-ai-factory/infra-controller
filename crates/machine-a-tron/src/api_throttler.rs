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
use std::collections::{HashMap, HashSet};

use carbide_uuid::machine::MachineId;
use rpc::Machine;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Interval;

use crate::api_client::ApiClient;
use crate::api_throttler::ApiCommand::GetMachine;

/// Returns an [`ApiThrottler`] which can expose certain API commands in a way that is throttled,
/// such that we only run them a maximum of once per time interval. Every interval, the pending
/// calls are coalesced into a single API call to save load on the server, and responses are
/// returned.
pub fn run(mut interval: Interval, api_client: ApiClient) -> ApiThrottler {
    let (message_tx, mut control_rx) = mpsc::unbounded_channel();
    tokio::task::Builder::new()
        .name("ApiThrottler")
        .spawn(async move {
            let mut get_machine_callers: HashMap<MachineId, Vec<oneshot::Sender<Option<Machine>>>> =
                HashMap::new();
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if !get_machine_callers.is_empty() {
                            // We have callers waiting to get a Machine by its ID. Translate the
                            // pending callers into a single GetMachinesByIds call and reply to each
                            // of them. Launder through a HashSet to de-dupe multiple calls for the
                            // same MachineId.
                            let machine_ids = get_machine_callers
                                .keys()
                                .collect::<HashSet<_>>()
                                .into_iter()
                                .collect::<Vec<_>>();

                            // Page the lookup so each reply stays well under tonic's
                            // 4 MiB receive limit: a page of 100 machines has exceeded
                            // it in the field at about 46 KB per machine.
                            const MACHINES_PAGE_SIZE: usize = 25;
                            let mut machines_by_id = HashMap::new();
                            for chunk in machine_ids.chunks(MACHINES_PAGE_SIZE) {
                                let machines = api_client.get_machines(
                                    chunk.iter().map(|id| **id).collect(),
                                )
                                .await
                                .inspect_err(|e| tracing::error!(
                                    error = %e,
                                    "API failure getting machines",
                                )).unwrap_or_default();

                                // Index the result by ID
                                for m in machines {
                                    if let Some(id) = m.id {
                                        machines_by_id.insert(id, m);
                                    }
                                }
                            }

                            // Reply to each
                            for (machine_id, replies) in get_machine_callers.into_iter() {
                                if let Some(machine) = machines_by_id.remove(&machine_id) {
                                    for reply in replies {
                                        _ = reply.send(Some(machine.clone()));
                                    }
                                } else {
                                    for reply in replies {
                                        _ = reply.send(None);
                                    }
                                }
                            }

                            // Clear the caller list
                            get_machine_callers = HashMap::new()
                        }
                    }
                    cmd = control_rx.recv() => {
                        let Some(cmd) = cmd else {
                            return;
                        };
                        match cmd {
                            ApiCommand::GetMachine(machine_id, reply) => get_machine_callers.entry(machine_id).or_default().push(reply),
                        };
                    }
                }
            }
        })
        .unwrap();
    ApiThrottler { message_tx }
}

enum ApiCommand {
    GetMachine(MachineId, oneshot::Sender<Option<Machine>>),
}

#[derive(Debug, Clone)]
pub struct ApiThrottler {
    message_tx: mpsc::UnboundedSender<ApiCommand>,
}

impl ApiThrottler {
    pub fn get_machine<F>(&self, machine_id: MachineId, completion: F)
    where
        F: Fn(Option<Machine>) + Send + Sync + 'static,
    {
        let (tx, rx) = oneshot::channel();
        _ = self.message_tx.send(GetMachine(machine_id, tx));
        // Get the API state in a background task, since we may be throttled for several seconds,
        // and callers shouldn't have to wait that whole time.
        tokio::spawn(async move {
            completion(rx.await.unwrap_or(None));
        });
    }
}
