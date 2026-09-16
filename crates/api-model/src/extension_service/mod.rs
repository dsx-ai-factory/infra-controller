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

use carbide_uuid::extension_service::ExtensionServiceId;
use chrono::prelude::*;
use config_version::{ConfigVersion, Versioned};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use super::tenant::TenantOrganizationId;
use crate::controller_outcome::PersistentStateHandlerOutcome;

/// The prefix used for the stable DPF resource and Helm release owned by an
/// extension service.
pub const DPF_HELM_CHART_NAME_PREFIX: &str = "extsvc-";
/// The NICo label namespace used for extension-service resources and placement.
pub const DPF_HELM_CHART_LABEL_PREFIX: &str = "nico/";
/// The label placed on a DPUService to identify its owning extension service.
pub const DPF_HELM_CHART_OWNER_LABEL: &str = "nico/extension-service-id";
/// The value used to enable a UUID-derived DPU placement label.
pub const DPF_HELM_CHART_PLACEMENT_LABEL_VALUE: &str = "enabled";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionServiceType {
    KubernetesPod,
    DpfHelmChart,
}

/// Address family required by one service-facing VPC interface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ServiceVpcAddressFamily {
    /// The service interface uses IPv4.
    Ipv4,
    /// The service interface uses IPv6.
    Ipv6,
}

/// Network requirement declared by an extension service.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceVpcInterfaceRequirement {
    /// Address family used by this interface.
    pub address_family: ServiceVpcAddressFamily,
}

/// Stable MAC assignment shared by every attachment of one service interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionServiceInterfaceMac {
    /// Service that owns this MAC assignment.
    pub service_id: ExtensionServiceId,
    /// Zero-based position in the service's interface requirements.
    pub interface_ordinal: u32,
    /// MAC reused for this service interface on every attached DPU.
    pub mac_address: MacAddress,
}

impl std::fmt::Display for ExtensionServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtensionServiceType::KubernetesPod => write!(f, "kubernetes_pod"),
            ExtensionServiceType::DpfHelmChart => write!(f, "dpf_helm_chart"),
        }
    }
}

#[derive(thiserror::Error, Debug, Clone)]
#[error("extension service type \"{0}\" is not valid")]
pub struct InvalidExtensionServiceTypeError(String);

impl std::str::FromStr for ExtensionServiceType {
    type Err = InvalidExtensionServiceTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "kubernetes_pod" => Ok(ExtensionServiceType::KubernetesPod),
            "dpf_helm_chart" => Ok(ExtensionServiceType::DpfHelmChart),
            _ => Err(InvalidExtensionServiceTypeError(s.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionService {
    pub id: ExtensionServiceId,
    pub service_type: ExtensionServiceType,
    pub name: String,
    pub tenant_organization_id: TenantOrganizationId,
    pub description: String,
    /// Service-facing interface requirements in ordinal order.
    #[serde(default)]
    pub service_vpc_interfaces: Vec<ServiceVpcInterfaceRequirement>,
    pub version_ctr: i32, // Version counter for the extension service, always incremented
    /// Controller-owned registration status. Kubernetes Pod services are
    /// synchronously ready, while DPF Helm services reconcile this lifecycle
    /// asynchronously.
    #[serde(skip, default)]
    pub status: ExtensionServiceStatus,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ExtensionServiceStatus {
    /// This version is independent of [`ExtensionService::version_ctr`]: it
    /// prevents a stale asynchronous controller iteration from overwriting a
    /// newer desired lifecycle request.
    pub controller_state: Versioned<ExtensionServiceLifecycleState>,
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
}

impl Default for ExtensionServiceStatus {
    fn default() -> Self {
        Self {
            controller_state: Versioned::new(
                ExtensionServiceLifecycleState::Ready,
                ConfigVersion::initial(),
            ),
            controller_state_outcome: None,
        }
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for ExtensionService {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let service_type_str: String = row.try_get("type")?;
        let service_type = service_type_str
            .parse::<ExtensionServiceType>()
            .map_err(|e| sqlx::Error::ColumnDecode {
                index: "type".to_string(),
                source: Box::new(e),
            })?;

        let tenant_organization_id: String = row.try_get("tenant_organization_id")?;
        let controller_state: sqlx::types::Json<ExtensionServiceLifecycleState> =
            row.try_get("controller_state")?;
        let controller_state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome")?;

        Ok(ExtensionService {
            id: row.try_get("id")?,
            service_type,
            name: row.try_get("name")?,
            tenant_organization_id: tenant_organization_id
                .parse::<TenantOrganizationId>()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            description: row.try_get("description")?,
            service_vpc_interfaces: row
                .try_get::<sqlx::types::Json<Vec<ServiceVpcInterfaceRequirement>>, _>(
                    "service_vpc_interfaces",
                )?
                .0,
            version_ctr: row.try_get::<i32, _>("version_ctr")?,
            status: ExtensionServiceStatus {
                controller_state: Versioned::new(
                    controller_state.0,
                    row.try_get("controller_state_version")?,
                ),
                controller_state_outcome: controller_state_outcome.map(|outcome| outcome.0),
            },
            created: row.try_get("created")?,
            updated: row.try_get("updated")?,
            deleted: row.try_get("deleted")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionServiceVersionInfo {
    pub service_id: ExtensionServiceId,
    pub version: ConfigVersion,
    pub created: DateTime<Utc>,
    pub data: String,
    pub observability: Option<ExtensionServiceObservability>,
    pub has_credential: bool,
    pub deleted: Option<DateTime<Utc>>,
}

impl<'r> sqlx::FromRow<'r, PgRow> for ExtensionServiceVersionInfo {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let obvs: Option<sqlx::types::Json<ExtensionServiceObservability>> =
            row.try_get("observability")?;

        Ok(ExtensionServiceVersionInfo {
            service_id: row.try_get("service_id")?,
            version: row.try_get("version")?,
            data: row.try_get("data")?,
            has_credential: row.try_get("has_credential")?,
            created: row.try_get("created")?,
            deleted: row.try_get("deleted")?,
            observability: obvs.map(|o| o.0),
        })
    }
}

/// A snapshot of the extension service information from DB that matches rpc::ExtensionService message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionServiceSnapshot {
    pub service_id: ExtensionServiceId,
    pub service_type: ExtensionServiceType,
    pub service_name: String,
    pub tenant_organization_id: TenantOrganizationId,
    pub version_ctr: i32,
    pub latest_version: Option<ExtensionServiceVersionInfo>,
    pub active_versions: Vec<ConfigVersion>,
    pub description: String,
    /// Service-facing interface requirements in ordinal order.
    pub service_vpc_interfaces: Vec<ServiceVpcInterfaceRequirement>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    /// Lifecycle state for the extension service registration.
    pub lifecycle_state: ExtensionServiceLifecycleState,
    pub lifecycle_state_version: ConfigVersion,
    pub lifecycle_state_outcome: Option<PersistentStateHandlerOutcome>,
}

impl<'r> FromRow<'r, PgRow> for ExtensionServiceSnapshot {
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        let service_id: ExtensionServiceId = row.try_get("service_id")?;
        let service_type_str: String = row.try_get("service_type")?;
        let service_type = service_type_str
            .parse::<ExtensionServiceType>()
            .map_err(|e| sqlx::Error::ColumnDecode {
                index: "type".to_string(),
                source: Box::new(e),
            })?;
        let service_name: String = row.try_get("service_name")?;
        let tenant_organization_id_str: String = row.try_get("tenant_organization_id")?;
        let tenant_organization_id: TenantOrganizationId = tenant_organization_id_str
            .parse::<TenantOrganizationId>()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let version_ctr: i32 = row.try_get("version_ctr")?;
        let description: String = row.try_get("description")?;
        let service_vpc_interfaces = row
            .try_get::<sqlx::types::Json<Vec<ServiceVpcInterfaceRequirement>>, _>(
                "service_vpc_interfaces",
            )?
            .0;
        let created: DateTime<Utc> = row.try_get("created")?;
        let updated: DateTime<Utc> = row.try_get("updated")?;
        let deleted: Option<DateTime<Utc>> = row.try_get("deleted")?;
        let lifecycle_state: Option<sqlx::types::Json<ExtensionServiceLifecycleState>> =
            row.try_get("controller_state")?;
        let lifecycle_state_version: ConfigVersion = row.try_get("controller_state_version")?;
        let lifecycle_state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome")?;

        let active_versions_str: Vec<String> = row.try_get("active_versions")?;
        let active_versions: Vec<ConfigVersion> = active_versions_str
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        let latest_version = row.try_get("latest_version")?;
        let latest_data = row.try_get("latest_data")?;
        let latest_has_credential = row.try_get("latest_has_credential")?;
        let latest_created = row.try_get("latest_created")?;

        let latest_observability: Option<sqlx::types::Json<ExtensionServiceObservability>> =
            row.try_get("latest_observability")?;

        let latest_service_version = match (
            latest_version,
            latest_data,
            latest_has_credential,
            latest_created,
        ) {
            (Some(version), Some(data), Some(has_credential), Some(created)) => {
                Some(ExtensionServiceVersionInfo {
                    service_id,
                    version,
                    data,
                    observability: latest_observability.map(|o| o.0),
                    has_credential,
                    created,
                    deleted: None,
                })
            }
            _ => None,
        };
        let lifecycle_state = lifecycle_state
            .map(|state| state.0)
            .ok_or_else(|| sqlx::Error::ColumnNotFound("controller_state".to_string()))?;

        Ok(ExtensionServiceSnapshot {
            service_id,
            service_type,
            service_name,
            tenant_organization_id,
            version_ctr,
            latest_version: latest_service_version,
            active_versions,
            description,
            service_vpc_interfaces,
            created,
            updated,
            deleted,
            lifecycle_state,
            lifecycle_state_version,
            lifecycle_state_outcome: lifecycle_state_outcome.map(|outcome| outcome.0),
        })
    }
}

/// Observability configuration options for an extension service version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtensionServiceObservabilityConfigTypePrometheus {
    pub scrape_interval_seconds: u32,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtensionServiceObservabilityConfigTypeLogging {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExtensionServiceObservabilityConfigType {
    Prometheus(ExtensionServiceObservabilityConfigTypePrometheus),
    Logging(ExtensionServiceObservabilityConfigTypeLogging),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtensionServiceObservabilityConfig {
    pub name: Option<String>,
    pub config: ExtensionServiceObservabilityConfigType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtensionServiceObservability {
    pub configs: Vec<ExtensionServiceObservabilityConfig>,
}

/// Immutable identity derived from an extension-service UUID for its DPF
/// resources and placement label.
///
/// `extsvc-` plus a canonical UUID is 43 characters.  That fits DPF's
/// 63-character DPUService name limit, Kubernetes' 63-character label-name
/// limit, and Helm's 53-character release-name limit without truncation or a
/// collision-prone hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpfHelmChartIdentity {
    pub dpu_service_name: String,
    pub helm_release_name: String,
    pub placement_label_key: String,
}

impl DpfHelmChartIdentity {
    pub fn from_service_id(service_id: ExtensionServiceId) -> Self {
        let dpu_service_name = format!("{DPF_HELM_CHART_NAME_PREFIX}{service_id}");
        Self {
            placement_label_key: format!("{DPF_HELM_CHART_LABEL_PREFIX}{dpu_service_name}"),
            helm_release_name: dpu_service_name.clone(),
            dpu_service_name,
        }
    }
}

/// Tenant-provided Helm chart data for a DPF Helm chart extension service.
///
/// This is intentionally the NICo API contract rather than a representation of
/// the DPUService CR.  NICo owns the remaining DPUService fields, including
/// the release name and placement selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DpfHelmChartServiceData {
    #[serde(rename = "repoURL")]
    pub repo_url: String,
    #[serde(rename = "chartName")]
    pub chart_name: String,
    #[serde(rename = "chartVersion")]
    pub chart_version: String,
    #[serde(rename = "security.privileged")]
    pub security_privileged: bool,
    /// Optional chart-specific values. When absent, no `helmChart.values`
    /// field is sent to DPF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum DpfHelmChartServiceDataError {
    #[error("invalid DPF helm chart data JSON: {0}")]
    Json(String),
    #[error("{0} must not be empty")]
    MissingField(&'static str),
    #[error("repoURL must begin with oci:// or https://")]
    InvalidRepositoryUrl,
    #[error("tenant values may not set NICo-owned field serviceDaemonSet.nodeSelector")]
    ReservedNodeSelector,
}

impl DpfHelmChartServiceData {
    /// Parses the complete Helm-service definition accepted by the NICo API.
    pub fn parse(data: &str) -> Result<Self, DpfHelmChartServiceDataError> {
        let parsed: Self = serde_json::from_str(data)
            .map_err(|error| DpfHelmChartServiceDataError::Json(error.to_string()))?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Produces a stable, normalized encoding suitable for durable desired
    /// state. `values` remains absent when the caller omitted it.
    pub fn normalized_json(&self) -> Result<String, DpfHelmChartServiceDataError> {
        serde_json::to_string(self)
            .map_err(|error| DpfHelmChartServiceDataError::Json(error.to_string()))
    }

    /// Parses a complete definition and returns the canonical JSON encoding
    /// that later persistence paths must store as desired state.
    pub fn parse_normalized(data: &str) -> Result<String, DpfHelmChartServiceDataError> {
        Self::parse(data)?.normalized_json()
    }

    fn validate(&self) -> Result<(), DpfHelmChartServiceDataError> {
        if self.repo_url.is_empty() {
            return Err(DpfHelmChartServiceDataError::MissingField("repoURL"));
        }
        if !(self.repo_url.starts_with("oci://") || self.repo_url.starts_with("https://")) {
            return Err(DpfHelmChartServiceDataError::InvalidRepositoryUrl);
        }
        if self.chart_name.is_empty() {
            return Err(DpfHelmChartServiceDataError::MissingField("chartName"));
        }
        if self.chart_version.is_empty() {
            return Err(DpfHelmChartServiceDataError::MissingField("chartVersion"));
        }
        if self.values.as_ref().is_some_and(|values| {
            values
                .get("serviceDaemonSet")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|service_daemon_set| service_daemon_set.contains_key("nodeSelector"))
        }) {
            return Err(DpfHelmChartServiceDataError::ReservedNodeSelector);
        }
        Ok(())
    }
}

/// Durable lifecycle state for an extension service managed by a state controller.
///
/// The responsible state controller alone advances this value after it has
/// reconciled the service's external resources.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ExtensionServiceLifecycleState {
    Creating,
    /// `active` was written before this state was renamed. Retain its input
    /// alias so historical backups remain readable; new records use `ready`.
    #[serde(rename = "ready", alias = "active")]
    Ready,
    Updating,
    Deleting,
    Deleted,
    Failed,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    // ExtensionServiceType parses case-insensitively from its wire form, and an
    // unknown string is rejected.
    #[test]
    fn extension_service_type_from_str() {
        scenarios!(
            run = |s| ExtensionServiceType::from_str(s).map_err(drop);
            "canonical kubernetes_pod" {
                "kubernetes_pod" => Yields(ExtensionServiceType::KubernetesPod),
            }

            "mixed case is normalized" {
                "Kubernetes_Pod" => Yields(ExtensionServiceType::KubernetesPod),
                "DPF_HELM_CHART" => Yields(ExtensionServiceType::DpfHelmChart),
            }

            "unknown type is rejected" {
                "virtual_machine" => Fails,
            }
        );
    }

    // Display is the inverse of from_str: each variant's wire form parses back to it.
    #[test]
    fn extension_service_type_display_round_trips() {
        scenarios!(
            run = |t| ExtensionServiceType::from_str(&t.to_string()).map_err(drop);
            "kubernetes_pod" {
                ExtensionServiceType::KubernetesPod => Yields(ExtensionServiceType::KubernetesPod),
            }

            "dpf_helm_chart" {
                ExtensionServiceType::DpfHelmChart => Yields(ExtensionServiceType::DpfHelmChart),
            }
        );
    }

    #[test]
    fn dpf_helm_chart_data_accepts_omitted_values() {
        let input = r#"{
                "chartVersion":"1.2.3",
                "security.privileged":false,
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service"
            }"#;
        let data = DpfHelmChartServiceData::parse(input).unwrap();

        assert_eq!(data.values, None);
        assert_eq!(
            data.normalized_json().unwrap(),
            r#"{"repoURL":"oci://registry.example.com/charts","chartName":"tenant-service","chartVersion":"1.2.3","security.privileged":false}"#
        );
        assert_eq!(
            DpfHelmChartServiceData::parse_normalized(input).unwrap(),
            data.normalized_json().unwrap()
        );
    }

    #[test]
    fn dpf_helm_chart_data_rejects_invalid_or_nico_owned_fields() {
        let required = r#"{
            "repoURL":"https://charts.example.com",
            "chartName":"tenant-service",
            "chartVersion":"1.2.3",
            "security.privileged":true,
            "values": %VALUES%
        }"#;

        assert!(matches!(
            DpfHelmChartServiceData::parse(&required.replace("%VALUES%", "[]")),
            Err(DpfHelmChartServiceDataError::Json(_))
        ));
        assert_eq!(
            DpfHelmChartServiceData::parse(
                &required.replace("%VALUES%", r#"{"serviceDaemonSet":{"nodeSelector":{}}}"#)
            ),
            Err(DpfHelmChartServiceDataError::ReservedNodeSelector)
        );
    }

    #[test]
    fn dpf_helm_chart_identity_uses_the_complete_service_uuid() {
        let service_id = ExtensionServiceId::from(
            uuid::Uuid::parse_str("f91aaea9-c117-4d6b-b744-635702317750").unwrap(),
        );

        let identity = DpfHelmChartIdentity::from_service_id(service_id);

        assert_eq!(
            identity.dpu_service_name,
            "extsvc-f91aaea9-c117-4d6b-b744-635702317750"
        );
        assert_eq!(identity.helm_release_name, identity.dpu_service_name);
        assert_eq!(
            identity.placement_label_key,
            "nico/extsvc-f91aaea9-c117-4d6b-b744-635702317750"
        );
        assert!(identity.dpu_service_name.len() <= 53);
        assert!(
            identity
                .placement_label_key
                .split_once('/')
                .unwrap()
                .1
                .len()
                <= 63
        );
    }

    #[test]
    fn dpf_helm_chart_lifecycle_states_have_a_stable_database_shape() {
        for (state, json) in [
            (
                ExtensionServiceLifecycleState::Creating,
                r#"{"state":"creating"}"#,
            ),
            (
                ExtensionServiceLifecycleState::Ready,
                r#"{"state":"ready"}"#,
            ),
            (
                ExtensionServiceLifecycleState::Updating,
                r#"{"state":"updating"}"#,
            ),
            (
                ExtensionServiceLifecycleState::Deleting,
                r#"{"state":"deleting"}"#,
            ),
            (
                ExtensionServiceLifecycleState::Deleted,
                r#"{"state":"deleted"}"#,
            ),
            (
                ExtensionServiceLifecycleState::Failed,
                r#"{"state":"failed"}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<ExtensionServiceLifecycleState>(json).unwrap(),
                state
            );
        }
    }

    // The observability config tree round-trips through JSON for each variant,
    // exercising the nested enum and its Prometheus / Logging payloads.
    #[test]
    fn observability_config_json_round_trip() {
        let prometheus = ExtensionServiceObservability {
            configs: vec![ExtensionServiceObservabilityConfig {
                name: Some("metrics".to_string()),
                config: ExtensionServiceObservabilityConfigType::Prometheus(
                    ExtensionServiceObservabilityConfigTypePrometheus {
                        scrape_interval_seconds: 30,
                        endpoint: "/metrics".to_string(),
                    },
                ),
            }],
        };
        let logging = ExtensionServiceObservability {
            configs: vec![ExtensionServiceObservabilityConfig {
                name: None,
                config: ExtensionServiceObservabilityConfigType::Logging(
                    ExtensionServiceObservabilityConfigTypeLogging {
                        path: "/var/log/svc.log".to_string(),
                    },
                ),
            }],
        };
        scenarios!(
            run = |obs| {
                let json = serde_json::to_string(&obs).map_err(drop)?;
                serde_json::from_str::<ExtensionServiceObservability>(&json).map_err(drop)
            };
            "prometheus config" {
                prometheus.clone() => Yields(prometheus),
            }

            "logging config" {
                logging.clone() => Yields(logging),
            }
        );
    }
}
