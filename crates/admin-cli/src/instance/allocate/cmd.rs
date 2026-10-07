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

use std::collections::{HashMap, VecDeque};

use carbide_uuid::machine::MachineId;

use super::args::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::machine;
use crate::rpc::ApiClient;

/// Batch-resolves an explicit `--machine-id` list into a lookup map via
/// chunked `find_machines_by_ids` calls, instead of leaving callers to
/// resolve each id with its own RPC. At fleet scale (thousands of ids) a
/// per-id resolution loop is itself enough sequential traffic to trip
/// per-client admission control, independent of whether the final allocate
/// call is transactional.
async fn prefetch_machines(
    api_client: &ApiClient,
    machine_ids: &[MachineId],
) -> CarbideCliResult<HashMap<MachineId, rpc::Machine>> {
    let chunk_size = api_client.effective_chunk_size(machine_ids.len()).await?;
    let mut machines = HashMap::with_capacity(machine_ids.len());
    for chunk in machine_ids.chunks(chunk_size.max(1)) {
        let list = api_client.get_machines_by_ids(chunk).await?;
        for machine in list.machines {
            if let Some(id) = machine.id {
                machines.insert(id, machine);
            }
        }
    }
    Ok(machines)
}

pub(super) async fn allocate(
    api_client: &ApiClient,
    allocate_request: Args,
    ctx: &RuntimeContext,
) -> CarbideCliResult<()> {
    let unsafe_op_msg = ctx.assert_cloud_unsafe_op_message()?;

    let number = allocate_request.number.unwrap_or(1);

    // Validate: --transactional requires --number > 1
    if allocate_request.transactional && number <= 1 {
        return Err(CarbideCliError::GenericError(
            "--transactional requires --number > 1".to_owned(),
        ));
    }

    // An explicit `--machine-id` list is fully known up front, so resolve it
    // in a handful of chunked calls instead of one RPC per machine -- at
    // fleet scale that per-machine loop is itself enough sequential traffic
    // to trip per-client admission control, regardless of `--transactional`.
    //
    // A transient failure partway through the prefetch (e.g. one chunked
    // `find_machines_by_ids` call erroring out) must not take down sequential
    // allocation the way it takes down transactional allocation: sequential
    // mode's whole contract is "keep going, best effort", and the pre-prefetch
    // code achieved that by resolving one machine at a time and treating a
    // failed lookup as an ineligible candidate rather than a fatal error. So
    // on a sequential request we fall back to `None` (per-machine lookup via
    // `get_next_free_machine`) instead of aborting the command; transactional
    // mode keeps propagating the error immediately since it's all-or-nothing
    // regardless.
    let prefetched_machines = if !allocate_request.machine_id.is_empty() {
        match prefetch_machines(api_client, &allocate_request.machine_id).await {
            Ok(machines) => Some(machines),
            Err(e) if allocate_request.transactional => return Err(e),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "prefetching explicit machine ids failed; falling back to per-machine lookup"
                );
                None
            }
        }
    } else {
        None
    };

    let mut machine_ids: VecDeque<_> = if !allocate_request.machine_id.is_empty() {
        allocate_request.machine_id.iter().copied().collect()
    } else {
        api_client
            .0
            .find_machine_ids(::rpc::forge::MachineSearchConfig {
                include_predicted_host: true,
                ..Default::default()
            })
            .await?
            .machine_ids
            .into()
    };

    let min_interface_count = if !allocate_request.vpc_prefix_id.is_empty() {
        allocate_request.vpc_prefix_id.len()
    } else {
        allocate_request.subnet.len()
    };

    if allocate_request.transactional {
        // Batch mode: all-or-nothing
        let mut requests = Vec::new();
        for i in 0..number {
            let next_machine = match &prefetched_machines {
                Some(cache) => {
                    machine::get_next_free_machine_prefetched(
                        api_client,
                        &mut machine_ids,
                        min_interface_count,
                        allocate_request.flat_vpc_id,
                        cache,
                    )
                    .await
                }
                None => {
                    machine::get_next_free_machine(
                        api_client,
                        &mut machine_ids,
                        min_interface_count,
                        allocate_request.flat_vpc_id,
                    )
                    .await
                }
            };
            let Some(machine) = next_machine else {
                return Err(CarbideCliError::GenericError(format!(
                    "Need {} machines but only {} available.",
                    number, i
                )));
            };

            let request = api_client
                .build_instance_request(
                    machine,
                    &allocate_request,
                    &format!("{}_{}", allocate_request.prefix_name, i),
                    Some(unsafe_op_msg.to_string()),
                )
                .await?;
            requests.push(request);
        }

        match api_client.allocate_instances(requests).await {
            Ok(instances) => {
                tracing::info!(
                    "Batch allocate was successful. Created {} instances.",
                    instances.len()
                );
                for instance in instances {
                    tracing::info!("  Created: {:?}", instance);
                }
            }
            Err(e) => {
                tracing::error!("Batch allocate failed: {}", e);
            }
        }
    } else {
        // Sequential mode: partial success allowed
        for i in 0..number {
            let next_machine = match &prefetched_machines {
                Some(cache) => {
                    machine::get_next_free_machine_prefetched(
                        api_client,
                        &mut machine_ids,
                        min_interface_count,
                        allocate_request.flat_vpc_id,
                        cache,
                    )
                    .await
                }
                None => {
                    machine::get_next_free_machine(
                        api_client,
                        &mut machine_ids,
                        min_interface_count,
                        allocate_request.flat_vpc_id,
                    )
                    .await
                }
            };
            let Some(machine) = next_machine else {
                tracing::error!("No available machines.");
                break;
            };

            match api_client
                .allocate_instance(
                    machine,
                    &allocate_request,
                    &format!("{}_{}", allocate_request.prefix_name, i),
                    Some(unsafe_op_msg.to_string()),
                )
                .await
            {
                Ok(i) => {
                    tracing::info!("allocate was successful. Created instance: {:?} ", i);
                }
                Err(e) => {
                    tracing::info!("allocate failed with {} ", e);
                }
            };
        }
    }
    Ok(())
}
