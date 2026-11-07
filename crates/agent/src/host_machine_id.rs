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
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
use carbide_host_support::agent_config::AgentConfig;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use forge_dpu_agent_utils::utils::create_forge_client;
use rpc::forge::MachineInterface;
use rpc::forge_tls_client::{ForgeClientConfig, ForgeClientT};

use crate::periodic_config_fetcher::PeriodicConfigFetcher;

async fn get_interface(
    client: &mut ForgeClientT,
    interface_id: MachineInterfaceId,
) -> Result<MachineInterface, eyre::Error> {
    let request = tonic::Request::new(rpc::forge::InterfaceSearchQuery {
        id: Some(interface_id),
        ip: None,
    });

    let mut interface_list = match client.find_interfaces(request).await {
        Ok(response) => Ok(response.into_inner()),
        Err(err) => Err(eyre::eyre!(
            "FindInterfaces gRPC request failed: interface_id={}, error={:?}",
            interface_id,
            err,
        )),
    }?;

    let len = interface_list.interfaces.len();
    if len != 1 {
        return Err(eyre::eyre!("expected exactly 1 interface, found {len}"));
    }
    Ok(interface_list.interfaces.remove(0))
}

async fn get_host_machine_id(
    agent_config: &AgentConfig,
    fetcher: &PeriodicConfigFetcher,
    forge_client_config: Arc<ForgeClientConfig>,
    forge_api: &str,
) -> Result<Option<MachineId>, eyre::Error> {
    // Try to get interface id from the agent config, otherwise try the periodic config fetcher.
    let interface_id_option = match agent_config.machine.interface_id {
        Some(id) => Some(id.into()),
        None => fetcher.get_host_machine_interface_id(),
    };

    if let Some(interface_id) = interface_id_option {
        let mut client = create_forge_client(forge_api, &forge_client_config).await?;
        let interface = get_interface(&mut client, interface_id).await?;
        return Ok(interface.machine_id);
    }

    Ok(None)
}

pub(super) async fn get_host_machine_id_retry(
    agent_config: &AgentConfig,
    fetcher: &PeriodicConfigFetcher,
    forge_client_config: Arc<ForgeClientConfig>,
    forge_api: &str,
) -> Result<MachineId, eyre::Report> {
    let retry_policy = ExponentialBuilder::default()
        .with_min_delay(Duration::from_millis(100))
        .with_max_delay(Duration::from_secs(10))
        .with_factor(2.0)
        .with_total_delay(Some(Duration::from_secs(10)));

    (|| async {
        get_host_machine_id(
            agent_config,
            fetcher,
            forge_client_config.clone(),
            forge_api,
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = ?e, "Failed to get host machine ID");
            e
        })?
        .ok_or(eyre::eyre!("get_host_machine_id() got no value"))
    })
    .retry(retry_policy)
    .await
}
