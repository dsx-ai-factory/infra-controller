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
use rpc::forge::{ExpectedPowerShelf, ExpectedPowerShelfRequest, PatchExpectedPowerShelfRequest};

use super::args::Args;
use crate::rpc::{ApiClient, maybe_unimplemented};

pub(super) async fn update(data: Args, api_client: &ApiClient) -> color_eyre::Result<()> {
    let paths = data.update_mask();
    let mut patch: ExpectedPowerShelf = data.clone().into();
    let selector = ExpectedPowerShelfRequest {
        bmc_mac_address: patch.bmc_mac_address.clone(),
        expected_power_shelf_id: patch.expected_power_shelf_id.clone(),
    };
    let current = if patch.expected_power_shelf_id.is_none() {
        let shelf = api_client
            .0
            .get_expected_power_shelf(selector.clone())
            .await
            .wrap_err("failed to resolve expected power shelf by BMC MAC address")?;
        patch.expected_power_shelf_id = shelf.expected_power_shelf_id.clone();
        Some(shelf)
    } else {
        None
    };

    // Core versions before shelf IDs must use the legacy MAC selector.
    if patch.expected_power_shelf_id.is_some() {
        let mut request = PatchExpectedPowerShelfRequest {
            expected_power_shelf: Some(patch),
            ..Default::default()
        };
        request.update_mask.get_or_insert_default().paths = paths;
        match api_client.0.patch_expected_power_shelf(request).await {
            Ok(()) => return Ok(()),
            Err(status) if maybe_unimplemented(&status) => {}
            Err(status) => return Err(status).wrap_err("failed to patch expected power shelf"),
        }
    }

    // The legacy RPC replaces the whole record and requires its stored MAC even
    // when selected by ID. Merge omitted fields before sending that replacement.
    let current = match current {
        Some(shelf) => shelf,
        None => api_client
            .0
            .get_expected_power_shelf(selector)
            .await
            .wrap_err("failed to get expected power shelf for legacy update")?,
    };
    api_client
        .0
        .update_expected_power_shelf(data.apply_to(current))
        .await
        .wrap_err("failed to update expected power shelf through the legacy RPC")
}
