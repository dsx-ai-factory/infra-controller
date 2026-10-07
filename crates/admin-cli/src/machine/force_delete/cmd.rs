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

use std::str::FromStr;
use std::time::Duration;

use ::rpc::forge::AdminForceDeleteMachineRequest;
use carbide_uuid::machine::MachineId;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn force_delete(mut query: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    const RETRY_TIME: Duration = Duration::from_secs(5);
    const MAX_WAIT_TIME: Duration = Duration::from_secs(60 * 20);

    let start = std::time::Instant::now();
    let mut dpu_machine_id = String::new();

    if let Ok(id) = MachineId::from_str(&query.machine)
        && api_client
            .0
            .find_instance_by_machine_id(id)
            .await
            .is_ok_and(|i| !i.instances.is_empty())
        && !query.allow_delete_with_instance
    {
        return Err(CarbideCliError::GenericError(
                "Machine has an associated instance, use --allow-delete-with-instance to acknowledge that this machine should be deleted with an instance allocated".to_string(),
            ));
    }

    loop {
        let req: AdminForceDeleteMachineRequest = (&query).into();
        let response = api_client.0.admin_force_delete_machine(req).await?;
        println!(
            "Force delete response: {}",
            serde_json::to_string_pretty(&response)?
        );

        if dpu_machine_id.is_empty() && !response.dpu_machine_id.is_empty() {
            dpu_machine_id = response.dpu_machine_id;
        }

        if response.all_done {
            println!("Force delete for {} succeeded", query.machine);

            // If we only searched for a Machine, then the DPU might be left behind
            // since the site controller can't look up the DPU by host machine ID anymore.
            // To also clean up the DPU, we modify our query and continue to delete
            if !dpu_machine_id.is_empty() && query.machine != dpu_machine_id {
                println!("Starting to delete potentially stale DPU machine {dpu_machine_id}");
                query.machine = dpu_machine_id.clone();
            } else {
                // No DPU to delete
                break;
            }
        }

        if start.elapsed() > MAX_WAIT_TIME {
            return Err(CarbideCliError::GenericError(format!(
                "Unable to force delete machine after {}s. Exiting",
                MAX_WAIT_TIME.as_secs()
            )));
        }

        println!(
            "Machine has not been fully deleted. Retrying after {}s",
            RETRY_TIME.as_secs()
        );
        tokio::time::sleep(RETRY_TIME).await;
    }

    Ok(())
}
