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

use super::args::{ForceClear, ForceSet};
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// A human-readable description of whichever UEFI target identifier(s) the
/// operator supplied, for the confirmation message.
fn describe_target(machine_id: Option<String>, bmc_mac: Option<String>) -> String {
    match (machine_id, bmc_mac) {
        (Some(id), Some(mac)) => format!("machine {id} (BMC {mac})"),
        (Some(id), None) => format!("machine {id}"),
        (None, Some(mac)) => format!("BMC {mac}"),
        (None, None) => "the requested machine".to_string(),
    }
}

pub(super) async fn set(data: ForceSet, api_client: &ApiClient) -> CarbideCliResult<()> {
    let target = describe_target(
        data.id.map(|id| id.to_string()),
        data.bmc_mac.map(|mac| mac.to_string()),
    );
    api_client.0.trigger_uefi_credential_rotation(data).await?;
    println!(
        "Requested force-converge of {target}'s UEFI credential. The state controller rotates it \
         on its next sweep (a BIOS job plus a host power-cycle, bypassing backoff); confirm this \
         device converged with `credential rotation-status --type=host_uefi --mac-address \
         <bmc-mac>` (the per-device query, not the site-wide view).",
    );
    Ok(())
}

pub(super) async fn clear(data: ForceClear, api_client: &ApiClient) -> CarbideCliResult<()> {
    let target = describe_target(
        data.id.map(|id| id.to_string()),
        data.bmc_mac.map(|mac| mac.to_string()),
    );
    api_client.0.trigger_uefi_credential_rotation(data).await?;
    println!("Cleared any pending UEFI force-converge request for {target}.");
    Ok(())
}
