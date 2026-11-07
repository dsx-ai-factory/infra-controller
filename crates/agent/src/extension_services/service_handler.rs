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
use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge as rpc;
use async_trait::async_trait;
use config_version::ConfigVersion;
use eyre::Result;

use crate::extension_services::dpu_extension_service_observability::DpuExtensionServiceObservability;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsernamePassword {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialType {
    UsernamePassword(UsernamePassword),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCredential {
    pub registry_url: String,
    pub credential_type: CredentialType,
}

/// Service instance with version information
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    pub id: uuid::Uuid,
    pub name: String,
    pub service_type: rpc::DpuExtensionServiceType,
    pub version: ConfigVersion,
    pub removed: Option<String>,
    pub data: String, // Service specification (e.g., pod spec YAML for pods)
    pub credential: Option<ServiceCredential>,
    pub observability: Option<DpuExtensionServiceObservability>,
}

impl TryFrom<rpc::ManagedHostDpuExtensionServiceConfig> for ServiceConfig {
    type Error = RpcDataConversionError;

    fn try_from(config: rpc::ManagedHostDpuExtensionServiceConfig) -> Result<Self, Self::Error> {
        let credential = config.credential.and_then(|c| match &c.r#type {
            Some(rpc::dpu_extension_service_credential::Type::UsernamePassword(up)) => {
                Some(ServiceCredential {
                    registry_url: c.registry_url,
                    credential_type: CredentialType::UsernamePassword(UsernamePassword {
                        username: up.username.clone(),
                        password: up.password.clone(),
                    }),
                })
            }
            None => None,
        });

        Ok(Self {
            id: uuid::Uuid::parse_str(&config.service_id).unwrap(),
            name: config.name,
            service_type: config
                .service_type
                .try_into()
                .unwrap_or(rpc::DpuExtensionServiceType::KubernetesPod),
            version: config.version.parse().map_err(|e| {
                RpcDataConversionError::InvalidConfigVersion(format!(
                    "Failed to parse version as ConfigVersion: {}",
                    e
                ))
            })?,
            observability: config.observability.map(|o| o.try_into()).transpose()?,
            removed: config.removed,
            data: config.data,
            credential,
        })
    }
}

/// Trait for handling different types of extension services
#[async_trait]
pub trait ExtensionServiceHandler: Send + Sync {
    // Deploy new set of the active services and terminate any that are no longer active
    async fn update_active_services(&mut self, service: &[ServiceConfig]) -> Result<()>;

    /// Get service status
    async fn get_service_status(
        &mut self,
        service: &ServiceConfig,
    ) -> Result<rpc::DpuExtensionServiceStatusObservation>;
}
