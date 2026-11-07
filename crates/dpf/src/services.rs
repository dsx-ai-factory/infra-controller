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

//! DPU service definitions (DTS, etc.) for DPUServiceTemplate and DPUServiceConfiguration.

use crate::types::{
    ConfigPortsServiceType, ServiceConfigPort, ServiceConfigPortProtocol, ServiceDefinition,
};

/// Default DOCA helm registry (DPUServiceTemplate source.repoURL).
pub const DEFAULT_DOCA_HELM_REGISTRY: &str = "https://helm.ngc.nvidia.com/nvidia/doca";

/// Overridable registry configuration for DPU services.
///
/// Allows callers to redirect helm chart sources for airgapped,
/// development, or mirrored environments.
#[derive(Debug, Clone)]
pub struct ServiceRegistryConfig {
    /// Helm chart repository URL for DOCA services (HBN, DTS).
    pub doca_helm_registry: String,
}

impl Default for ServiceRegistryConfig {
    fn default() -> Self {
        Self {
            doca_helm_registry: DEFAULT_DOCA_HELM_REGISTRY.to_string(),
        }
    }
}

/// DTS (Doca Telemetry Service) service definition.
pub fn dts_service(reg: &ServiceRegistryConfig) -> ServiceDefinition {
    ServiceDefinition {
        helm_values: Some(serde_json::json!({
            "exposedPorts": { "ports": { "httpserverport": true } },
            "serviceDaemonSet": {
                "resources": {
                    "requests": { "memory": "320Mi", "cpu": "200m" },
                    "limits":   { "memory": "320Mi", "cpu": "1" }
                }
            }

        })),
        config_ports: Some(vec![ServiceConfigPort {
            name: "httpserverport".to_string(),
            port: 9189,
            protocol: ServiceConfigPortProtocol::Tcp,
            node_port: None,
        }]),
        config_ports_service_type: Some(ConfigPortsServiceType::ClusterIp),
        ..ServiceDefinition::new("dts", &reg.doca_helm_registry, "doca-telemetry", "1.22.1")
    }
}

/// Default DPU services. Used when `config.services` is empty.
pub fn default_services(reg: &ServiceRegistryConfig) -> Vec<ServiceDefinition> {
    vec![dts_service(reg)]
}
