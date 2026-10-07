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

pub(crate) mod args;
mod cmds;

#[cfg(test)]
mod tests;

use args::Cmd;
use librms::RackManagerClientPool;

use crate::cfg::cli_options::CliOptions;
use crate::rms::args::RmsAction;

// `rms` is intentionally OUTSIDE the `Dispatch`/`Run` trait flow: like `redfish`,
// it builds its own client (an RMS rack-manager pool) from raw `CliOptions` and
// is dispatched by `main` *before* the API client / `RuntimeContext` exists --
// see the `CliCommand::Rms` branch in `main.rs`. The traits carry a
// `RuntimeContext` rms never has, so it stays a plain `action` fn rather than
// implementing them. Please don't "realign" it onto the traits.
pub(crate) async fn action(action: RmsAction, config: &CliOptions) -> color_eyre::Result<()> {
    let url = if let Some(x) = action.url {
        x
    } else if let Some(y) = config.rms_api_url.clone() {
        y
    } else {
        eprintln!("No RMS API URL provided.");
        return Ok(());
    };
    let root_ca = if let Some(x) = action.root_ca {
        Some(x)
    } else {
        config.rms_root_ca_path.clone()
    };
    let client_cert = if let Some(x) = action.client_cert {
        Some(x)
    } else {
        config.rms_client_cert_path.clone()
    };
    let client_key = if let Some(x) = action.client_key {
        Some(x)
    } else {
        config.rms_client_key_path.clone()
    };
    let enforce_tls = !(root_ca.is_none() || client_cert.is_none() || client_key.is_none());

    // similar to libredfish
    let rms_client_config =
        librms::client_config::RmsClientConfig::new(root_ca, client_cert, client_key, enforce_tls);
    let rms_api_config = librms::client::RmsApiConfig::new(&url, &rms_client_config);
    let rms_client_pool = librms::RmsClientPool::new(&rms_api_config);
    let rms_client = rms_client_pool.create_client().await;

    match action.command {
        Cmd::Inventory => cmds::list_node_inventory(&rms_client).await,
        Cmd::PowerOnSequence(args) => cmds::power_on_sequence(args, &rms_client).await,
        Cmd::PowerState(args) => cmds::power_state(args, &rms_client).await,
        Cmd::FirmwareInventory(args) => cmds::get_firmware_inventory(args, &rms_client).await,
    }
}
