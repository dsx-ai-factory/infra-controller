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

use ::ufm_mock::{InventoryProvider, UfmAuthToken, UfmMock, UfmMockConfig};
use axum::Router;
use machine_a_tron::ControlState;
use metrics_endpoint::{
    MetricsEndpointConfig, MetricsSetup, new_metrics_setup, run_metrics_endpoint_with_cancellation,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(super) struct HostedUfmMock {
    router: Router,
    cancellation: CancellationToken,
    reconciliation: JoinHandle<()>,
    metrics_task: Option<JoinHandle<()>>,
    metrics_setup: MetricsSetup,
}

impl HostedUfmMock {
    pub(super) fn start(
        config: Option<UfmMockConfig>,
        control_state: &ControlState,
    ) -> eyre::Result<Option<Self>> {
        let Some(config) = config.filter(|config| config.enabled) else {
            return Ok(None);
        };

        tracing::info!("Enabling hosted UFM mock");
        let auth_token = UfmAuthToken::from_environment()?;
        let metrics_setup = new_metrics_setup("carbide-ufm-mock", "carbide", false)?;
        let ufm = UfmMock::new(&config, &auth_token, &metrics_setup.meter)?;
        let local_provider = config
            .include_local_inventory
            .then(|| Arc::new(control_state.clone()) as Arc<dyn InventoryProvider>);
        let cancellation = CancellationToken::new();
        let reconciliation =
            ufm.start_reconciliation(config.inventory, local_provider, cancellation.child_token());
        let metrics_task = config.metrics_address.map(|address| {
            let metrics_config = MetricsEndpointConfig {
                address,
                registry: metrics_setup.registry.clone(),
                health_controller: Some(metrics_setup.health_controller.clone()),
                additional_prefix: None,
            };
            let cancellation = cancellation.child_token();
            tokio::spawn(async move {
                if let Err(error) =
                    run_metrics_endpoint_with_cancellation(&metrics_config, cancellation).await
                {
                    tracing::error!(error = %error, "Hosted UFM metrics endpoint stopped");
                }
            })
        });

        Ok(Some(Self {
            router: ufm.router(),
            cancellation,
            reconciliation,
            metrics_task,
            metrics_setup,
        }))
    }

    pub(super) fn router(&self) -> Router {
        self.router.clone()
    }

    pub(super) async fn shutdown(self) -> eyre::Result<()> {
        self.cancellation.cancel();
        self.reconciliation.await?;
        if let Some(metrics_task) = self.metrics_task {
            metrics_task.await?;
        }
        drop(self.metrics_setup);
        Ok(())
    }
}
