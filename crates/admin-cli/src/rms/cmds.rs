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

use std::sync::Arc;

use librms::RmsApi;

use crate::rms::args::{FirmwareInventory, PowerOnSequence, PowerState};

/// Print the RMS node inventory as JSON.
pub(super) async fn list_node_inventory(rms_client: &Arc<dyn RmsApi>) -> eyre::Result<()> {
    let response = rms_client.list_node_inventory().await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

pub(super) async fn power_on_sequence(
    args: PowerOnSequence,
    rms_client: &Arc<dyn RmsApi>,
) -> eyre::Result<()> {
    let response = rms_client.get_rack_power_on_sequence(args.into()).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

pub(super) async fn power_state(
    args: PowerState,
    rms_client: &Arc<dyn RmsApi>,
) -> eyre::Result<()> {
    let response = rms_client.get_power_state(args.into()).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

pub(super) async fn get_firmware_inventory(
    args: FirmwareInventory,
    rms_client: &Arc<dyn RmsApi>,
) -> eyre::Result<()> {
    let response = rms_client.get_node_firmware_inventory(args.into()).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
