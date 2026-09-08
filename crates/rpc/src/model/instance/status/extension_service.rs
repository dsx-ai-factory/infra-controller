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

use std::time::SystemTime;

use carbide_uuid::extension_service::ExtensionServiceId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use model::instance::status::extension_service::{
    ExtensionServiceComponent, ExtensionServiceDeploymentStatus, ExtensionServiceStatusObservation,
    InstanceExtensionServiceStatus, InstanceExtensionServiceStatusObservation,
    InstanceExtensionServicesStatus, MachineExtensionServiceStatus,
};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<InstanceExtensionServicesStatus> for rpc::InstanceDpuExtensionServicesStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceExtensionServicesStatus) -> Result<Self, Self::Error> {
        let mut extension_services = Vec::with_capacity(status.extension_services.len());
        for service in status.extension_services {
            extension_services.push(rpc::InstanceDpuExtensionServiceStatus::try_from(service)?);
        }
        Ok(rpc::InstanceDpuExtensionServicesStatus {
            dpu_extension_services: extension_services,
            configs_synced: rpc::SyncState::try_from(status.configs_synced)? as i32,
        })
    }
}

impl From<rpc::DpuExtensionServiceDeploymentStatus> for ExtensionServiceDeploymentStatus {
    fn from(status: rpc::DpuExtensionServiceDeploymentStatus) -> Self {
        match status {
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceUnknown => Self::Unknown,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending => Self::Pending,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning => Self::Running,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating => {
                Self::Terminating
            }
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminated => {
                Self::Terminated
            }
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceFailed => Self::Failed,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceError => Self::Error,
        }
    }
}

impl From<ExtensionServiceDeploymentStatus> for rpc::DpuExtensionServiceDeploymentStatus {
    fn from(status: ExtensionServiceDeploymentStatus) -> Self {
        match status {
            ExtensionServiceDeploymentStatus::Unknown => Self::DpuExtensionServiceUnknown,
            ExtensionServiceDeploymentStatus::Pending => Self::DpuExtensionServicePending,
            ExtensionServiceDeploymentStatus::Running => Self::DpuExtensionServiceRunning,
            ExtensionServiceDeploymentStatus::Terminating => Self::DpuExtensionServiceTerminating,
            ExtensionServiceDeploymentStatus::Terminated => Self::DpuExtensionServiceTerminated,
            ExtensionServiceDeploymentStatus::Failed => Self::DpuExtensionServiceFailed,
            ExtensionServiceDeploymentStatus::Error => Self::DpuExtensionServiceError,
        }
    }
}

impl TryFrom<rpc::DpuExtensionServiceComponent> for ExtensionServiceComponent {
    type Error = RpcDataConversionError;

    fn try_from(component: rpc::DpuExtensionServiceComponent) -> Result<Self, Self::Error> {
        Ok(Self {
            name: component.name,
            version: component.version,
            url: component.url,
            status: component.status,
        })
    }
}

impl From<ExtensionServiceComponent> for rpc::DpuExtensionServiceComponent {
    fn from(component: ExtensionServiceComponent) -> Self {
        Self {
            name: component.name,
            version: component.version,
            url: component.url,
            status: component.status,
        }
    }
}

impl TryFrom<MachineExtensionServiceStatus> for rpc::DpuExtensionServiceStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: MachineExtensionServiceStatus) -> Result<Self, Self::Error> {
        Ok(Self {
            dpu_machine_id: Some(status.machine_id.into()),
            status: rpc::DpuExtensionServiceDeploymentStatus::from(status.status).into(),
            error_message: status.error_message,
            components: status
                .components
                .into_iter()
                .map(rpc::DpuExtensionServiceComponent::from)
                .collect(),
        })
    }
}

impl TryFrom<InstanceExtensionServiceStatus> for rpc::InstanceDpuExtensionServiceStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceExtensionServiceStatus) -> Result<Self, Self::Error> {
        let dpu_statuses = status
            .dpu_statuses
            .into_iter()
            .map(rpc::DpuExtensionServiceStatus::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            service_id: status.service_id.into(),
            version: status.version.to_string(),
            deployment_status: rpc::DpuExtensionServiceDeploymentStatus::from(
                status.overall_status,
            )
            .into(),
            dpu_statuses,
            removed: status.removed,
        })
    }
}

impl TryFrom<rpc::DpuExtensionServiceStatusObservation> for ExtensionServiceStatusObservation {
    type Error = RpcDataConversionError;

    fn try_from(
        observation: rpc::DpuExtensionServiceStatusObservation,
    ) -> Result<Self, Self::Error> {
        let service_id = observation
            .service_id
            .parse::<ExtensionServiceId>()
            .map_err(|e| {
                RpcDataConversionError::InvalidUuid("ExtensionServiceId", e.to_string())
            })?;

        let service_type = rpc::DpuExtensionServiceType::try_from(observation.service_type)
            .map_err(|_| {
                RpcDataConversionError::InvalidValue(
                    observation.service_type.to_string(),
                    "service_type".to_string(),
                )
            })?
            .into();

        let overall_state = rpc::DpuExtensionServiceDeploymentStatus::try_from(observation.state)
            .map_err(|_| {
                RpcDataConversionError::InvalidValue(
                    observation.state.to_string(),
                    "state".to_string(),
                )
            })?
            .into();

        let components = observation
            .components
            .into_iter()
            .map(ExtensionServiceComponent::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        let version = observation.version.parse::<ConfigVersion>().map_err(|e| {
            RpcDataConversionError::InvalidConfigVersion(format!(
                "Failed to parse version as ConfigVersion: {}",
                e
            ))
        })?;

        Ok(Self {
            service_id,
            service_type,
            service_name: observation.service_name,
            version,
            removed: observation.removed,
            overall_state,
            components,
            message: observation.message,
        })
    }
}

/// Converts the legacy DPU-agent extension-service report into the standard
/// instance extension-service observation. The caller determines whether an
/// observation exists from `dpu_extension_service_version`; an omitted version
/// deliberately remains an absent observation rather than a parse error.
impl TryFrom<&rpc::DpuNetworkStatus> for InstanceExtensionServiceStatusObservation {
    type Error = RpcDataConversionError;

    fn try_from(observation: &rpc::DpuNetworkStatus) -> Result<Self, Self::Error> {
        let version_string = observation.dpu_extension_service_version.as_ref().ok_or(
            Self::Error::MissingArgument("dpu_extension_service_version"),
        )?;
        let config_version = version_string.as_str().parse().map_err(|_| {
            RpcDataConversionError::InvalidConfigVersion(format!(
                "applied_config.extension_service_version: {version_string}"
            ))
        })?;
        let instance_config_version = observation
            .instance_config_version
            .as_ref()
            .map(|version_string| {
                version_string.as_str().parse().map_err(|_| {
                    RpcDataConversionError::InvalidConfigVersion(format!(
                        "applied_config.instance_config_version: {version_string}"
                    ))
                })
            })
            .transpose()?;
        let observed_at = match observation.observed_at {
            Some(timestamp) => DateTime::from(
                SystemTime::try_from(timestamp)
                    .map_err(|_| RpcDataConversionError::InvalidTimestamp(timestamp.to_string()))?,
            ),
            None => Utc::now(),
        };

        Ok(Self {
            config_version,
            instance_config_version,
            extension_service_statuses: observation
                .dpu_extension_services
                .iter()
                .cloned()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            observed_at,
        })
    }
}

impl From<ExtensionServiceStatusObservation> for rpc::DpuExtensionServiceStatusObservation {
    fn from(observation: ExtensionServiceStatusObservation) -> Self {
        Self {
            service_id: observation.service_id.into(),
            service_type: rpc::DpuExtensionServiceType::from(observation.service_type).into(),
            service_name: observation.service_name,
            version: observation.version.to_string(),
            removed: observation.removed,
            state: rpc::DpuExtensionServiceDeploymentStatus::from(observation.overall_state).into(),
            components: observation
                .components
                .into_iter()
                .map(|c| rpc::DpuExtensionServiceComponent {
                    name: c.name,
                    version: c.version,
                    url: c.url,
                    status: c.status,
                })
                .collect(),
            message: observation.message,
        }
    }
}
