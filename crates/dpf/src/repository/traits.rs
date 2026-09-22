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

//! Repository traits for DPF CRD operations.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;

use crate::crds::bfbs_generated::BFB;
use crate::crds::bluefieldsoftwares_generated::BlueFieldSoftware;
use crate::crds::dpfoperatorconfigs_generated::DPFOperatorConfig;
use crate::crds::dpuclusters_generated::DPUCluster;
use crate::crds::dpudeployments_generated::DPUDeployment;
use crate::crds::dpudevices_generated::DPUDevice;
use crate::crds::dpuflavors_generated::DPUFlavor;
use crate::crds::dpuflavortemplates_generated::DPUFlavorTemplate;
use crate::crds::dpunodemaintenances_generated::DPUNodeMaintenance;
use crate::crds::dpunodes_generated::DPUNode;
use crate::crds::dpus_generated::DPU;
use crate::crds::dpuservicechains_generated::DPUServiceChain;
use crate::crds::dpuserviceconfigurations_generated::DPUServiceConfiguration;
use crate::crds::dpuserviceinterfaces_generated::DPUServiceInterface;
use crate::crds::dpuservicenads_generated::DPUServiceNAD;
use crate::crds::dpuservices_generated::DPUService;
use crate::crds::dpuservicetemplates_generated::DPUServiceTemplate;
use crate::crds::dpusets_generated::DPUSet;
use crate::error::DpfError;

/// Repository for BFB (BlueField Bundle) resources.
#[async_trait]
pub trait BfbRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<BFB>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<BFB>, DpfError>;
    async fn create(&self, bfb: &BFB) -> Result<BFB, DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;
}

/// Repository for BlueFieldSoftware resources.
///
/// BF4-class DPUs are provisioned from a `BlueFieldSoftware` CR (OS ISO +
/// firmware bundle) rather than a BFB, so this repository mirrors
/// [`BfbRepository`] for that resource.
#[async_trait]
pub trait BlueFieldSoftwareRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str)
    -> Result<Option<BlueFieldSoftware>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<BlueFieldSoftware>, DpfError>;
    async fn create(&self, bfs: &BlueFieldSoftware) -> Result<BlueFieldSoftware, DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;
}

/// Repository for DPU resources.
#[async_trait]
pub trait DpuRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPU>, DpfError>;
    async fn list(
        &self,
        namespace: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<DPU>, DpfError>;
    async fn patch_status(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;

    /// Delete a DPU only when its Kubernetes UID matches `uid`.
    ///
    /// The UID check must be part of the delete operation so a DPU recreated
    /// under the same name cannot be deleted using an earlier observation.
    async fn delete_if_uid(&self, name: &str, namespace: &str, uid: &str) -> Result<(), DpfError>;

    /// Watch for DPU changes and invoke the handler for each object.
    ///
    /// The returned future runs until the watch is cancelled. The handler's
    /// `Result` signals whether the event was processed; implementations may
    /// retry on `Err`.
    ///
    /// When `label_selector` is `Some`, only DPU resources matching the
    /// selector are watched.
    fn watch<F, Fut>(
        &self,
        namespace: &str,
        label_selector: Option<&str>,
        handler: F,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        F: Fn(Arc<DPU>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static;
}

/// Repository for DPUDevice resources.
#[async_trait]
pub trait DpuDeviceRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUDevice>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUDevice>, DpfError>;
    async fn create(&self, device: &DPUDevice) -> Result<DPUDevice, DpfError>;
    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;
}

/// Repository for DPUNode resources.
#[async_trait]
pub trait DpuNodeRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUNode>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUNode>, DpfError>;
    async fn create(&self, node: &DPUNode) -> Result<DPUNode, DpfError>;
    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;
}

/// Repository for DPUNodeMaintenance resources.
#[async_trait]
pub trait DpuNodeMaintenanceRepository: Send + Sync {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUNodeMaintenance>, DpfError>;
    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;
}

/// Repository for DPUFlavor resources.
#[async_trait]
pub trait DpuFlavorRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUFlavor>, DpfError>;
    async fn create(&self, flavor: &DPUFlavor) -> Result<DPUFlavor, DpfError>;
}

/// Repository for DPUFlavorTemplate resources.
#[async_trait]
pub trait DpuFlavorTemplateRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str)
    -> Result<Option<DPUFlavorTemplate>, DpfError>;
    async fn create(&self, template: &DPUFlavorTemplate) -> Result<DPUFlavorTemplate, DpfError>;
}

/// Repository for DPUSet resources.
#[async_trait]
pub trait DpuSetRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUSet>, DpfError>;
    async fn apply(&self, set: &DPUSet) -> Result<DPUSet, DpfError>;
}

/// Repository for DPUCluster resources.
#[async_trait]
pub trait DpuClusterRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUCluster>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUCluster>, DpfError>;
}

/// Repository for DPUDeployment resources.
#[async_trait]
pub trait DpuDeploymentRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUDeployment>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUDeployment>, DpfError>;
    async fn apply(&self, deployment: &DPUDeployment) -> Result<DPUDeployment, DpfError>;
    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;
}

/// Repository for DPUServiceTemplate resources.
#[async_trait]
pub trait DpuServiceTemplateRepository: Send + Sync {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUServiceTemplate>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceTemplate>, DpfError>;
    async fn apply(&self, template: &DPUServiceTemplate) -> Result<DPUServiceTemplate, DpfError>;
}

/// Repository for DPUServiceConfiguration resources.
#[async_trait]
pub trait DpuServiceConfigurationRepository: Send + Sync {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUServiceConfiguration>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceConfiguration>, DpfError>;
    async fn apply(
        &self,
        config: &DPUServiceConfiguration,
    ) -> Result<DPUServiceConfiguration, DpfError>;
}

/// Repository for DPUService resources.
#[async_trait]
pub trait DpuServiceRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUService>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUService>, DpfError>;
    async fn create(&self, service: &DPUService) -> Result<DPUService, DpfError>;
    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;
}

/// Repository for DPUServiceNAD resources.
#[async_trait]
pub trait DpuServiceNADRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUServiceNAD>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceNAD>, DpfError>;
    async fn apply(&self, nad: &DPUServiceNAD) -> Result<DPUServiceNAD, DpfError>;
}

/// Repository for DPUServiceChain resources.
#[async_trait]
pub trait DpuServiceChainRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUServiceChain>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceChain>, DpfError>;
}

/// Repository for DPUServiceInterface resources.
#[async_trait]
pub trait DpuServiceInterfaceRepository: Send + Sync {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUServiceInterface>, DpfError>;
    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceInterface>, DpfError>;
    async fn apply(&self, iface: &DPUServiceInterface) -> Result<DPUServiceInterface, DpfError>;
    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError>;
}

/// Repository for Kubernetes ConfigMaps and Secrets.
#[async_trait]
pub trait K8sConfigRepository: Send + Sync {
    async fn get_configmap(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DpfError>;
    /// Create a ConfigMap, reporting `false` when one already exists.
    ///
    /// Distinct from `apply_configmap`, which force-applies and would overwrite
    /// content the object already holds.
    async fn create_configmap(
        &self,
        name: &str,
        namespace: &str,
        data: BTreeMap<String, String>,
    ) -> Result<bool, DpfError>;

    async fn apply_configmap(
        &self,
        name: &str,
        namespace: &str,
        data: BTreeMap<String, String>,
    ) -> Result<(), DpfError>;
    async fn get_secret(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, DpfError>;
    /// Create the Secret, or overwrite the data of an existing one. Callers
    /// rewrite this Secret when the underlying credential rotates, so an
    /// implementation that skips existing objects would silently pin the
    /// cluster to the first value ever written.
    async fn apply_secret(
        &self,
        name: &str,
        namespace: &str,
        data: BTreeMap<String, Vec<u8>>,
    ) -> Result<(), DpfError>;
}

/// Repository for DPFOperatorConfig resources.
#[async_trait]
pub trait DpfOperatorConfigRepository: Send + Sync {
    async fn get(&self, name: &str, namespace: &str)
    -> Result<Option<DPFOperatorConfig>, DpfError>;

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError>;
}

/// Combined trait for all DPF repository operations.
///
/// Implementors of this trait provide access to all DPF CRD operations,
/// enabling the SDK to work with any backend (real K8s, mock, etc.).
pub trait DpfRepository:
    BfbRepository
    + BlueFieldSoftwareRepository
    + DpuRepository
    + DpuDeviceRepository
    + DpuNodeRepository
    + DpuNodeMaintenanceRepository
    + DpuFlavorRepository
    + DpuFlavorTemplateRepository
    + DpuSetRepository
    + DpuClusterRepository
    + DpuDeploymentRepository
    + DpuServiceTemplateRepository
    + DpuServiceConfigurationRepository
    + DpuServiceRepository
    + DpuServiceChainRepository
    + DpuServiceInterfaceRepository
    + K8sConfigRepository
    + DpfOperatorConfigRepository
    + Send
    + Sync
{
}
