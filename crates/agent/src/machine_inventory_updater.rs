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

use ::rpc::forge as rpc;
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use carbide_uuid::machine::DpuMachineId;

use crate::command_line::AgentPlatformType;
use crate::containerd::container;
use crate::containerd::container::ContainerSummary;
use crate::instrumentation::InventoryReport;

#[derive(Debug, Clone)]
pub(super) struct MachineInventoryUpdaterConfig {
    pub(super) dpu_agent_version: String,
    /// How often to update the inventory
    pub(super) update_inventory_interval: Duration,
    pub(super) machine_id: DpuMachineId,
    pub(super) forge_api: String,
    pub(super) forge_client_config: Arc<ForgeClientConfig>,
    pub(super) agent_platform_type: AgentPlatformType,
}

pub(super) async fn single_run(config: &MachineInventoryUpdaterConfig) -> eyre::Result<()> {
    // Measure the whole iteration: the container and image lookups below can
    // fail with `?` before the report RPC, and those failures must count too.
    let result: eyre::Result<()> = async {
        tracing::trace!(
            machine_id = %config.machine_id,
            "Updating machine inventory"
        );

        let machine_id = config.machine_id;

        // We won't be able to see these containers unless we're in the DPU OS.
        let result = if config.agent_platform_type.is_dpu_os() {
            let containers = container::Containers::list().await?;

            let images = container::Images::list().await?;

            tracing::trace!(?containers, "Containers");

            let mut result: Vec<ContainerSummary> = Vec::new();

            // Map container images to container names
            for mut c in containers.containers {
                let images_clone = images.clone();
                let images_names = images_clone.find_by_id(&c.image.id)?;
                c.image_ref = images_names.names;
                result.push(c);
            }
            result
        } else {
            vec![]
        };

        let mut inventory: Vec<rpc::MachineInventorySoftwareComponent> = result
            .into_iter()
            .flat_map(|c| {
                c.image_ref
                    .into_iter()
                    .map(|n| rpc::MachineInventorySoftwareComponent {
                        name: n.name.clone(),
                        version: n.version.clone(),
                        url: n.repository,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        // Add the DPU agent version to the inventory
        inventory.push(rpc::MachineInventorySoftwareComponent {
            name: "forge-dpu-agent".to_string(),
            version: config.dpu_agent_version.clone(),
            url: String::new(),
        });

        let inventory = rpc::MachineInventory {
            components: inventory,
        };

        let agent_report = rpc::DpuAgentInventoryReport {
            machine_id: Some(machine_id),
            inventory: Some(inventory),
        };

        update_agent_reported_inventory(
            agent_report,
            &config.forge_client_config,
            &config.forge_api,
        )
        .await
    }
    .await;

    // `single_run` returns failures to the scheduler, which logs the error.
    // Keep `InventoryReport::Failed` metric-only to avoid a duplicate
    // diagnostic; `InventoryReport::Succeeded` owns the DEBUG record.
    if result.is_ok() {
        InventoryReport::Succeeded.emit();
    } else {
        InventoryReport::Failed.emit();
    }

    result
}

async fn update_agent_reported_inventory(
    inventory_report: rpc::DpuAgentInventoryReport,
    client_config: &forge_tls_client::ForgeClientConfig,
    forge_api: &str,
) -> eyre::Result<()> {
    let mut client = match forge_tls_client::ForgeTlsClient::retry_build(&ApiConfig::new(
        forge_api,
        client_config,
    ))
    .await
    {
        Ok(client) => client,
        Err(err) => {
            return Err(eyre::eyre!(
                "could not connect to forge API server at {}: {err}",
                forge_api
            ));
        }
    };

    let software_component_count = inventory_report
        .inventory
        .as_ref()
        .map_or(0, |inventory| inventory.components.len());
    tracing::trace!(
        machine_id = ?inventory_report.machine_id,
        software_component_count,
        "Updating machine inventory"
    );

    let request = tonic::Request::new(inventory_report);
    match client.update_agent_reported_inventory(request).await {
        Ok(response) => {
            tracing::trace!(
                ?response,
                "Received agent-reported inventory update response"
            );
            Ok(())
        }
        Err(err) => Err(eyre::eyre!(
            "error while executing the update_agent_reported_inventory gRPC call: {}",
            err.to_string()
        )),
    }
}
