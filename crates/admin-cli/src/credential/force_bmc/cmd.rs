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
use carbide_uuid::device::DeviceId;
use mac_address::MacAddress;

use super::args::{ForceClear, ForceSet};
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// A human-readable description of whichever BMC identifier(s) the operator
/// supplied, for the confirmation message.
fn describe_target(device_id: Option<DeviceId>, bmc_mac: Option<MacAddress>) -> String {
    match (device_id, bmc_mac) {
        (Some(DeviceId::Machine(id)), Some(mac)) => format!("machine {id} (BMC {mac})"),
        (Some(DeviceId::Machine(id)), None) => format!("machine {id}"),
        (Some(DeviceId::Switch(id)), Some(mac)) => format!("switch {id} (BMC {mac})"),
        (Some(DeviceId::Switch(id)), None) => format!("switch {id}"),
        (Some(DeviceId::PowerShelf(id)), Some(mac)) => format!("power shelf {id} (BMC {mac})"),
        (Some(DeviceId::PowerShelf(id)), None) => format!("power shelf {id}"),
        (None, Some(mac)) => format!("BMC {mac}"),
        (None, None) => "the requested BMC".to_string(),
    }
}

pub(super) async fn set(data: ForceSet, api_client: &ApiClient) -> CarbideCliResult<()> {
    let target = describe_target(data.id, data.bmc_mac);
    api_client.0.trigger_bmc_credential_rotation(data).await?;
    println!(
        "Requested force-converge of {target}. The state controller rotates it on its next \
         sweep (bypassing backoff); confirm this device converged with \
         `credential rotation-status --type=bmc --mac-address <bmc-mac>` (the per-device query, \
         not the site-wide view).",
    );
    Ok(())
}

pub(super) async fn clear(data: ForceClear, api_client: &ApiClient) -> CarbideCliResult<()> {
    let target = describe_target(data.id, data.bmc_mac);
    api_client.0.trigger_bmc_credential_rotation(data).await?;
    println!("Cleared any pending BMC force-converge request for {target}.");
    Ok(())
}
