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
use std::collections::HashMap;

use ::rpc::forge::{self as rpc, DpuExtensionServiceType};

use super::k8s_pod_handler::KubernetesPodServicesHandler;
use super::service_handler::{ExtensionServiceHandler, ServiceConfig};
use crate::command_line::AgentPlatformType;

/// Manager for all extension services on the DPU
///
/// This manager is responsible for updating the desired services and getting the statuses of the services.
pub struct ExtensionServiceManager {
    service_handlers: HashMap<DpuExtensionServiceType, Box<dyn ExtensionServiceHandler>>,
}

impl ExtensionServiceManager {
    pub fn platform_defaults(agent_platform_type: &AgentPlatformType) -> Self {
        let mut service_handlers = HashMap::new();
        match agent_platform_type {
            AgentPlatformType::DpuOs => {
                service_handlers.insert(
                    DpuExtensionServiceType::KubernetesPod,
                    Box::new(KubernetesPodServicesHandler::default())
                        as Box<dyn ExtensionServiceHandler>,
                );
            }
            AgentPlatformType::Containerized => {
                // We can't support the KubernetesPod or
                // KubernetesPodServicesHandler handlers as currently
                // implemented (they do a lot of raw crictl operations), so
                // there's nothing in here.
            }
        }

        Self { service_handlers }
    }
}

impl ExtensionServiceManager {
    fn get_handler_mut(
        &mut self,
        t: &DpuExtensionServiceType,
    ) -> eyre::Result<&mut dyn ExtensionServiceHandler> {
        self.service_handlers
            .get_mut(t)
            .map(|handler| handler.as_mut() as &mut dyn ExtensionServiceHandler)
            .ok_or_else(|| {
                eyre::eyre!(
                    "extension service type {:?} is not supported on this DPU",
                    t
                )
            })
    }

    pub async fn update_desired_services(
        &mut self,
        configs: Vec<rpc::ManagedHostDpuExtensionServiceConfig>,
    ) -> eyre::Result<()> {
        let mut services_by_type: HashMap<DpuExtensionServiceType, Vec<ServiceConfig>> =
            HashMap::new();

        for config in configs {
            let service = ServiceConfig::try_from(config).map_err(|e| {
                eyre::eyre!(
                    "failed to convert ManagedHostDpuExtensionServiceConfig to ServiceConfig: {}",
                    e
                )
            })?;
            services_by_type
                .entry(service.service_type)
                .or_default()
                .push(service);
        }

        for (service_type, handler) in self.service_handlers.iter_mut() {
            // Note if no service configs are provided for a given service type, the handler still needs to be called
            // to ensure all services are cleaned up.
            let desired_services = services_by_type.remove(service_type).unwrap_or_default();
            handler.update_active_services(&desired_services).await?;
        }

        Ok(())
    }

    pub async fn get_service_statuses(
        &mut self,
        configs: Vec<rpc::ManagedHostDpuExtensionServiceConfig>,
    ) -> eyre::Result<Vec<rpc::DpuExtensionServiceStatusObservation>> {
        let service_configs: Vec<ServiceConfig> = configs
            .into_iter()
            .map(|c| {
                ServiceConfig::try_from(c).map_err(|e| eyre::eyre!(
                "failed to convert ManagedHostDpuExtensionServiceConfig to ServiceConfig: {e}"
            ))
            })
            .collect::<Result<_, _>>()?;

        let mut service_statuses = Vec::with_capacity(service_configs.len());
        for service in service_configs {
            let status = match self.get_handler_mut(&service.service_type) {
                Ok(handler) => handler.get_service_status(&service).await?,
                Err(err) => {
                    let state = if service.removed.is_some() {
                        tracing::info!(
                            service_id = %service.id,
                            error = %err,
                            "Treating removed unsupported extension service as terminated"
                        );
                        rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminated
                    } else {
                        tracing::error!(
                            service_id = %service.id,
                            error = %err,
                            "Error getting extension service status"
                        );
                        rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceError
                    };
                    rpc::DpuExtensionServiceStatusObservation {
                        service_id: service.id.to_string(),
                        service_type: service.service_type.into(),
                        service_name: service.name,
                        version: service.version.to_string(),
                        removed: service.removed,
                        state: state.into(),
                        components: vec![],
                        message: err.to_string(),
                    }
                }
            };
            service_statuses.push(status);
        }

        Ok(service_statuses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_config(removed: Option<String>) -> rpc::ManagedHostDpuExtensionServiceConfig {
        rpc::ManagedHostDpuExtensionServiceConfig {
            service_id: uuid::Uuid::nil().to_string(),
            name: "test-service".to_string(),
            version: config_version::ConfigVersion::initial().version_string(),
            service_type: DpuExtensionServiceType::KubernetesPod.into(),
            removed,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn containerized_platform_reports_status_for_unsupported_services() {
        let mut manager =
            ExtensionServiceManager::platform_defaults(&AgentPlatformType::Containerized);

        let statuses = manager
            .get_service_statuses(vec![
                service_config(None),
                service_config(Some(chrono::Utc::now().to_rfc3339())),
            ])
            .await
            .expect("unsupported services should produce status observations");

        assert_eq!(statuses.len(), 2);
        assert_eq!(
            statuses[0].state,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceError as i32
        );
        assert_eq!(
            statuses[1].state,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminated as i32
        );
        assert!(statuses[1].removed.is_some());
    }
}
