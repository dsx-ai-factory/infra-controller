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

use ::rpc::forge::SpdmMachineAttestationTriggerRequest;

use crate::attestation::spdm::trigger::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn trigger(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let res = api_client
        .0
        .trigger_machine_attestation(SpdmMachineAttestationTriggerRequest {
            machine_id: Some(args.machine_id),
            redfish_timeout_secs: args.redfish_timeout_secs,
        })
        .await?;

    let machine_id = res
        .machine_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    if let Some(result) = res.result {
        let job_id = result
            .job_id
            .map(|id| id.value)
            .unwrap_or_else(|| "none".to_string());
        println!(
            "Attestation triggered for machine {machine_id}: {} device(s), job ID {job_id}",
            result.devices_under_attestation
        );
    } else {
        println!("No supported devices found for SPDM attestation on machine {machine_id}");
    }

    Ok(())
}
