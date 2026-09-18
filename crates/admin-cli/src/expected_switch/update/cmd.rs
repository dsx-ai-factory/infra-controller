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

use eyre::WrapErr;
use rpc::forge::{ExpectedSwitch, ExpectedSwitchRequest, PatchExpectedSwitchRequest};
use rpc::forge_api_client::expected_switch_update_mask;

use super::args::Args;
use crate::rpc::{ApiClient, maybe_unimplemented};

pub(super) async fn update(data: Args, api_client: &ApiClient) -> color_eyre::Result<()> {
    let legacy: ExpectedSwitch = data.try_into()?;
    let update_mask = expected_switch_update_mask(&legacy);
    // Keep the caller's selector for the legacy RPC; PATCH requires its ID.
    let mut patch = legacy.clone();
    if patch.expected_switch_id.is_none() {
        let switch = api_client
            .0
            .get_expected_switch(ExpectedSwitchRequest {
                bmc_mac_address: patch.bmc_mac_address.clone(),
                expected_switch_id: None,
            })
            .await
            .wrap_err("failed to resolve expected switch by BMC MAC address")?;
        patch.expected_switch_id = switch.expected_switch_id;
    }

    // Core versions before switch IDs must use the legacy MAC selector.
    if patch.expected_switch_id.is_some() {
        let mut request = PatchExpectedSwitchRequest {
            expected_switch: Some(patch),
            ..Default::default()
        };
        request.update_mask.get_or_insert_default().paths =
            update_mask.iter().map(|field| field.to_string()).collect();
        match api_client.0.patch_expected_switch(request).await {
            Ok(()) => return Ok(()),
            Err(status) if maybe_unimplemented(&status) => {}
            Err(status) => return Err(status).wrap_err("failed to patch expected switch"),
        }
    }

    api_client
        .0
        .update_expected_switch_with_mask(legacy, &update_mask)
        .await
        .wrap_err("failed to update expected switch through the legacy RPC")
}
