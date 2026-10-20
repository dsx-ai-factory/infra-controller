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

//! Kubernetes implementation of the DPF repository traits.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PostParams, Preconditions};
use kube::runtime::controller::Action;
use kube::runtime::{Controller, watcher};
use kube::{Api, Client, Resource};
use tokio_util::sync::CancellationToken;

use super::traits::*;
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
use crate::types::DpfDeletePreconditions;

/// Kubernetes-backed implementation of DPF repository.
#[derive(Clone)]
pub struct KubeRepository {
    client: Client,
    cancel: CancellationToken,
}

impl KubeRepository {
    /// Create a new KubeRepository with the default in-cluster or kubeconfig client.
    pub async fn new() -> Result<Self, DpfError> {
        let client = Client::try_default().await?;
        Ok(Self {
            client,
            cancel: CancellationToken::new(),
        })
    }

    /// Create a new KubeRepository with a provided client.
    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            cancel: CancellationToken::new(),
        }
    }

    /// Get a reference to the underlying Kubernetes client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn api<K>(&self, namespace: &str) -> Api<K>
    where
        K: Resource<Scope = kube::core::NamespaceResourceScope>,
        <K as Resource>::DynamicType: Default,
    {
        Api::namespaced(self.client.clone(), namespace)
    }

    /// Converts only Kubernetes HTTP 409 responses into PatchConflict so handlers retry stale
    /// resource versions without retrying permanent validation failures.
    fn json_patch_error(error: kube::Error, kind: &'static str, name: &str) -> DpfError {
        match error {
            kube::Error::Api(status) if status.is_conflict() => DpfError::PatchConflict {
                kind,
                name: name.to_owned(),
            },
            error => error.into(),
        }
    }
}

#[async_trait]
impl BfbRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<BFB>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<BFB>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn create(&self, bfb: &BFB) -> Result<BFB, DpfError> {
        let namespace = bfb.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), bfb).await?)
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<BFB> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }
}

#[async_trait]
impl BlueFieldSoftwareRepository for KubeRepository {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<BlueFieldSoftware>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<BlueFieldSoftware>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn create(&self, bfs: &BlueFieldSoftware) -> Result<BlueFieldSoftware, DpfError> {
        let namespace = bfs.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), bfs).await?)
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<BlueFieldSoftware> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }
}

#[async_trait]
impl DpuRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPU>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(
        &self,
        namespace: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<DPU>, DpfError> {
        let api = self.api(namespace);
        let params = match label_selector {
            Some(sel) => ListParams::default().labels(sel),
            None => ListParams::default(),
        };
        let list = api.list(&params).await?;
        Ok(list.items)
    }

    async fn patch_status(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        let api: Api<DPU> = self.api(namespace);
        api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPU> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }

    async fn delete_if_uid(&self, name: &str, namespace: &str, uid: &str) -> Result<(), DpfError> {
        let api: Api<DPU> = self.api(namespace);
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(uid.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        api.delete(name, &params).await?;
        Ok(())
    }

    fn watch<F, Fut>(
        &self,
        namespace: &str,
        label_selector: Option<&str>,
        handler: F,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        F: Fn(Arc<DPU>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        let api: Api<DPU> = self.api(namespace);
        let cancel = self.cancel.clone();
        let watcher_config = match label_selector {
            Some(selector) => watcher::Config::default().labels(selector),
            None => watcher::Config::default(),
        };
        async move {
            Controller::new(api, watcher_config)
                .graceful_shutdown_on(cancel.cancelled_owned())
                .run(
                    |obj: Arc<DPU>, ctx: Arc<F>| async move {
                        ctx(obj).await?;
                        // eventual consistency, recommended by kube-runtime in case of watch desync
                        Ok(Action::requeue(jitter(Duration::from_secs(3600), 0.2)))
                    },
                    |_obj: Arc<DPU>, error: &DpfError, _ctx: Arc<F>| {
                        tracing::warn!(error = %error, "DPU watch handler failed, requeuing");
                        Action::requeue(jitter(Duration::from_secs(30), 0.2))
                    },
                    Arc::new(handler),
                )
                .for_each(|res| async {
                    if let Err(e) = res {
                        tracing::warn!(error = %e, "DPU reconciliation error");
                    }
                })
                .await;
        }
    }
}

#[async_trait]
impl DpuDeviceRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUDevice>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUDevice>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn create(&self, device: &DPUDevice) -> Result<DPUDevice, DpfError> {
        let namespace = device.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), device).await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        let api: Api<DPUDevice> = self.api(namespace);
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPUDevice> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }
}

#[async_trait]
impl DpuNodeRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUNode>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUNode>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn create(&self, node: &DPUNode) -> Result<DPUNode, DpfError> {
        let namespace = node.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), node).await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        let api: Api<DPUNode> = self.api(namespace);
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPUNode> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }
}

#[async_trait]
impl DpuNodeMaintenanceRepository for KubeRepository {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUNodeMaintenance>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        let api: Api<DPUNodeMaintenance> = self.api(namespace);
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }
}

#[async_trait]
impl DpuFlavorRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUFlavor>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn create(&self, flavor: &DPUFlavor) -> Result<DPUFlavor, DpfError> {
        let namespace = flavor.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), flavor).await?)
    }
}

#[async_trait]
impl DpuFlavorTemplateRepository for KubeRepository {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUFlavorTemplate>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn create(&self, template: &DPUFlavorTemplate) -> Result<DPUFlavorTemplate, DpfError> {
        let namespace = template.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), template).await?)
    }
}

#[async_trait]
impl DpuSetRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUSet>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn apply(&self, set: &DPUSet) -> Result<DPUSet, DpfError> {
        let namespace = set.meta().namespace.as_deref().unwrap_or("default");
        let name = set.meta().name.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api
            .patch(
                name,
                &PatchParams::apply("carbide-dpf-sdk").force(),
                &Patch::Apply(set),
            )
            .await?)
    }
}

#[async_trait]
impl DpuClusterRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUCluster>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUCluster>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }
}

#[async_trait]
impl DpuDeploymentRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUDeployment>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUDeployment>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn apply(&self, deployment: &DPUDeployment) -> Result<DPUDeployment, DpfError> {
        let namespace = deployment.meta().namespace.as_deref().unwrap_or("default");
        let name = deployment.meta().name.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api
            .patch(
                name,
                &PatchParams::apply("carbide-dpf-sdk").force(),
                &Patch::Apply(deployment),
            )
            .await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        let api: Api<DPUDeployment> = self.api(namespace);
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPUDeployment> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }
}

#[async_trait]
impl DpuServiceTemplateRepository for KubeRepository {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUServiceTemplate>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceTemplate>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn apply(&self, template: &DPUServiceTemplate) -> Result<DPUServiceTemplate, DpfError> {
        let namespace = template.meta().namespace.as_deref().unwrap_or("default");
        let name = template.meta().name.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api
            .patch(
                name,
                &PatchParams::apply("carbide-dpf-sdk").force(),
                &Patch::Apply(template),
            )
            .await?)
    }
}

#[async_trait]
impl DpuServiceConfigurationRepository for KubeRepository {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUServiceConfiguration>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceConfiguration>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn apply(
        &self,
        config: &DPUServiceConfiguration,
    ) -> Result<DPUServiceConfiguration, DpfError> {
        let namespace = config.meta().namespace.as_deref().unwrap_or("default");
        let name = config.meta().name.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api
            .patch(
                name,
                &PatchParams::apply("carbide-dpf-sdk").force(),
                &Patch::Apply(config),
            )
            .await?)
    }
}

#[async_trait]
impl DpuServiceNADRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUServiceNAD>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceNAD>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn apply(&self, nad: &DPUServiceNAD) -> Result<DPUServiceNAD, DpfError> {
        let namespace = nad.meta().namespace.as_deref().unwrap_or("default");
        let name = nad.meta().name.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api
            .patch(
                name,
                &PatchParams::apply("carbide-dpf-sdk").force(),
                &Patch::Apply(nad),
            )
            .await?)
    }
}

#[async_trait]
impl DpuServiceRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUService>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUService>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
    }

    async fn create(&self, service: &DPUService) -> Result<DPUService, DpfError> {
        let namespace = service.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), service).await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<DPUService, DpfError> {
        let api: Api<DPUService> = self.api(namespace);
        let patch = serde_json::from_value::<json_patch::Patch>(patch)?;
        api.patch(name, &PatchParams::default(), &Patch::Json::<()>(patch))
            .await
            .map_err(|error| Self::json_patch_error(error, "DPUService", name))
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPUService> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }

    async fn delete_if_current(
        &self,
        name: &str,
        namespace: &str,
        preconditions: &DpfDeletePreconditions,
    ) -> Result<(), DpfError> {
        let api: Api<DPUService> = self.api(namespace);
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(preconditions.uid.clone()),
                resource_version: Some(preconditions.resource_version.clone()),
            }),
            ..Default::default()
        };
        match api.delete(name, &params).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait]
impl DpuServiceChainRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUServiceChain>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(
        &self,
        namespace: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<DPUServiceChain>, DpfError> {
        let api = self.api(namespace);
        let params = match label_selector {
            Some(selector) => ListParams::default().labels(selector),
            None => ListParams::default(),
        };
        let list = api.list(&params).await?;
        Ok(list.items)
    }

    async fn create(&self, chain: &DPUServiceChain) -> Result<DPUServiceChain, DpfError> {
        let namespace = chain.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), chain).await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<DPUServiceChain, DpfError> {
        let api: Api<DPUServiceChain> = self.api(namespace);
        let patch = serde_json::from_value::<json_patch::Patch>(patch)?;
        api.patch(name, &PatchParams::default(), &Patch::Json::<()>(patch))
            .await
            .map_err(|error| Self::json_patch_error(error, "DPUServiceChain", name))
    }

    async fn delete_if_current(
        &self,
        name: &str,
        namespace: &str,
        preconditions: &DpfDeletePreconditions,
    ) -> Result<(), DpfError> {
        let api: Api<DPUServiceChain> = self.api(namespace);
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(preconditions.uid.clone()),
                resource_version: Some(preconditions.resource_version.clone()),
            }),
            ..Default::default()
        };
        match api.delete(name, &params).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait]
impl DpuServiceInterfaceRepository for KubeRepository {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPUServiceInterface>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(
        &self,
        namespace: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<DPUServiceInterface>, DpfError> {
        let api = self.api(namespace);
        let params = match label_selector {
            Some(selector) => ListParams::default().labels(selector),
            None => ListParams::default(),
        };
        let list = api.list(&params).await?;
        Ok(list.items)
    }

    async fn apply(&self, iface: &DPUServiceInterface) -> Result<DPUServiceInterface, DpfError> {
        let namespace = iface.meta().namespace.as_deref().unwrap_or("default");
        let name = iface.meta().name.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api
            .patch(
                name,
                &PatchParams::apply("carbide-dpf-sdk").force(),
                &Patch::Apply(iface),
            )
            .await?)
    }

    async fn create(&self, iface: &DPUServiceInterface) -> Result<DPUServiceInterface, DpfError> {
        let namespace = iface.meta().namespace.as_deref().unwrap_or("default");
        let api = self.api(namespace);
        Ok(api.create(&PostParams::default(), iface).await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<DPUServiceInterface, DpfError> {
        let api: Api<DPUServiceInterface> = self.api(namespace);
        let patch = serde_json::from_value::<json_patch::Patch>(patch)?;
        api.patch(name, &PatchParams::default(), &Patch::Json::<()>(patch))
            .await
            .map_err(|error| Self::json_patch_error(error, "DPUServiceInterface", name))
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPUServiceInterface> = self.api(namespace);
        match api.delete(name, &Default::default()).await {
            Ok(_) => {}
            Err(kube::Error::Api(error)) if error.code == 404 => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn delete_if_current(
        &self,
        name: &str,
        namespace: &str,
        preconditions: &DpfDeletePreconditions,
    ) -> Result<(), DpfError> {
        let api: Api<DPUServiceInterface> = self.api(namespace);
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(preconditions.uid.clone()),
                resource_version: Some(preconditions.resource_version.clone()),
            }),
            ..Default::default()
        };
        match api.delete(name, &params).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait]
impl K8sConfigRepository for KubeRepository {
    async fn get_configmap(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DpfError> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        match api.get_opt(name).await? {
            Some(cm) => Ok(cm.data),
            None => Ok(None),
        }
    }

    async fn create_configmap(
        &self,
        name: &str,
        namespace: &str,
        data: BTreeMap<String, String>,
    ) -> Result<bool, DpfError> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let cm = ConfigMap {
            metadata: kube::core::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            data: Some(data),
            ..Default::default()
        };
        match api.create(&PostParams::default(), &cm).await {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(err)) if err.is_already_exists() => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    async fn apply_configmap(
        &self,
        name: &str,
        namespace: &str,
        data: BTreeMap<String, String>,
    ) -> Result<(), DpfError> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let cm = ConfigMap {
            metadata: kube::core::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            data: Some(data),
            ..Default::default()
        };
        api.patch(
            name,
            &PatchParams::apply("carbide-dpf-sdk").force(),
            &Patch::Apply(&cm),
        )
        .await?;
        Ok(())
    }

    async fn get_secret(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, DpfError> {
        let api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        match api.get_opt(name).await? {
            Some(secret) => Ok(secret
                .data
                .map(|d| d.into_iter().map(|(k, v)| (k, v.0)).collect())),
            None => Ok(None),
        }
    }

    async fn apply_secret(
        &self,
        name: &str,
        namespace: &str,
        data: BTreeMap<String, Vec<u8>>,
    ) -> Result<(), DpfError> {
        let api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        let secret = Secret {
            metadata: kube::core::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            data: Some(
                data.into_iter()
                    .map(|(k, v)| (k, k8s_openapi::ByteString(v)))
                    .collect(),
            ),
            ..Default::default()
        };
        // Server-side apply, matching `apply_configmap`: creates the Secret when
        // absent and overwrites the data when present, so a rotated credential
        // actually reaches the cluster.
        api.patch(
            name,
            &PatchParams::apply("carbide-dpf-sdk").force(),
            &Patch::Apply(&secret),
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl DpfOperatorConfigRepository for KubeRepository {
    async fn get(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<DPFOperatorConfig>, DpfError> {
        let api: Api<DPFOperatorConfig> = Api::namespaced(self.client.clone(), namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn patch(
        &self,
        name: &str,
        namespace: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        let api: Api<DPFOperatorConfig> = Api::namespaced(self.client.clone(), namespace);
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }
}

// Implement the meta trait
impl DpfRepository for KubeRepository {}

/// Apply +/- `fraction` uniform random jitter to a base duration.
fn jitter(base: Duration, fraction: f64) -> Duration {
    let secs = base.as_secs_f64();
    let jittered = rand::random_range(secs * (1.0 - fraction)..secs * (1.0 + fraction));
    Duration::from_secs_f64(jittered)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use http::{Method, Request, Response, StatusCode};
    use kube::Client;
    use kube::client::Body;
    use serde_json::json;
    use tower_test::mock;

    use super::KubeRepository;
    use crate::crds::dpuservicechains_generated::DPUServiceChain;
    use crate::crds::dpuserviceinterfaces_generated::DPUServiceInterface;
    use crate::error::DpfError;
    use crate::repository::{
        DpuServiceChainRepository, DpuServiceInterfaceRepository, DpuServiceRepository,
    };
    use crate::types::DpfDeletePreconditions;

    /// Builds the smallest checked interface resource needed to verify repository transport.
    /// Uses a generated DPUServiceInterface to prove POST serialization matches the CR schema.
    fn detached_interface() -> DPUServiceInterface {
        serde_json::from_value(json!({
            "apiVersion": "svc.dpu.nvidia.com/v1alpha1",
            "kind": "DPUServiceInterface",
            "metadata": {"name": "service-interface", "namespace": "test-ns"},
            "spec": {
                "template": {
                    "metadata": {"labels": {"owner": "id"}},
                    "spec": {
                        "nodeSelector": {"matchLabels": {"node": "enabled"}},
                        "template": {
                            "metadata": {"labels": {"owner": "id"}},
                            "spec": {
                                "interfaceType": "patch",
                                "patch": {"peerBridge": "svcbr0"},
                            },
                        },
                    },
                },
            },
        }))
        .expect("interface fixture follows the generated CRD schema")
    }

    /// Builds the smallest checked chain resource needed to verify repository transport.
    /// A real generated type makes the POST proof cover selector, port, and MTU serialization.
    fn detached_chain() -> DPUServiceChain {
        serde_json::from_value(json!({
            "apiVersion": "svc.dpu.nvidia.com/v1alpha1",
            "kind": "DPUServiceChain",
            "metadata": {"name": "service-chain", "namespace": "test-ns"},
            "spec": {
                "template": {
                    "metadata": {"labels": {"owner": "id"}},
                    "spec": {
                        "nodeSelector": {"matchLabels": {"node": "enabled"}},
                        "template": {
                            "metadata": {"labels": {"owner": "id"}},
                            "spec": {
                                "switches": [{
                                    "ports": [{"serviceInterface": {"matchLabels": {"owner": "id"}}}],
                                    "serviceMTU": 1500,
                                }],
                            },
                        },
                    },
                },
            },
        }))
        .expect("chain fixture follows the generated CRD schema")
    }

    /// Verifies DPUService patches use RFC 6902, classify HTTP 409 as retryable conflict, preserve
    /// HTTP 422 as validation failure, and return the patched object for convergence checks.
    #[tokio::test]
    async fn dpu_service_patch_uses_json_patch_media_type() {
        let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
        let repository = KubeRepository::with_client(Client::new(service, "default"));
        let patch = json!([
            {"op": "replace", "path": "/metadata/resourceVersion", "value": "17"},
            {"op": "add", "path": "/spec/helmChart/values", "value": {"featureOverride": null}},
        ]);

        // Classify HTTP 409 as a DPUService patch conflict so reconciliation can retry with a fresh
        // resource version.
        let update =
            DpuServiceRepository::patch(&repository, "extension-service", "test-ns", patch.clone());
        let inspect_request = async {
            let (request, send_response) = handle
                .next_request()
                .await
                .expect("DPUService patch must send one Kubernetes request");
            assert_eq!(request.method(), Method::PATCH);
            assert_eq!(
                request.uri(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuservices/extension-service"
            );
            assert_eq!(
                request.headers().get(http::header::CONTENT_TYPE).unwrap(),
                "application/json-patch+json"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                patch
            );
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "status": "Failure",
                            "message": "resource version conflict",
                            "reason": "Conflict",
                            "code": 409,
                        })
                        .to_string()
                        .into_bytes(),
                    ))
                    .unwrap(),
            );
        };

        let (update_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(update, inspect_request)
        })
        .await
        .expect("DPUService patch request must complete");
        assert!(matches!(
            update_result,
            Err(DpfError::PatchConflict { kind: "DPUService", ref name })
                if name == "extension-service"
        ));

        // Preserve HTTP 422 as a permanent validation error because replaying an invalid patch
        // cannot repair the request.
        let update =
            DpuServiceRepository::patch(&repository, "extension-service", "test-ns", patch.clone());
        let inspect_request = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::PATCH);
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::UNPROCESSABLE_ENTITY)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "status": "Failure",
                            "message": "invalid resource quantity",
                            "reason": "Invalid",
                            "code": 422,
                        })
                        .to_string()
                        .into_bytes(),
                    ))
                    .unwrap(),
            );
        };
        let (update_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(update, inspect_request)
        })
        .await
        .expect("DPUService validation failure must complete");
        assert!(matches!(
            update_result,
            Err(DpfError::KubeError(kube::Error::Api(error))) if error.code == 422
        ));

        // A successful PATCH returns the resulting object so callers can validate actual
        // convergence.
        let update =
            DpuServiceRepository::patch(&repository, "extension-service", "test-ns", patch.clone());
        let inspect_request = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::PATCH);
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                patch
            );
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "apiVersion": "svc.dpu.nvidia.com/v1alpha1",
                            "kind": "DPUService",
                            "metadata": {
                                "name": "extension-service",
                                "namespace": "test-ns",
                                "resourceVersion": "18",
                            },
                            "spec": {
                                "deployInCluster": false,
                                "helmChart": {
                                    "source": {
                                        "chart": "extension",
                                        "repoURL": "oci://registry.example.com/extensions",
                                        "version": "1.0.0",
                                    },
                                    "values": {"featureOverride": null},
                                },
                            },
                        })
                        .to_string()
                        .into_bytes(),
                    ))
                    .unwrap(),
            );
        };
        let (updated, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(update, inspect_request)
        })
        .await
        .expect("DPUService patch response must complete");
        assert_eq!(
            updated.unwrap().metadata.resource_version.as_deref(),
            Some("18")
        );
    }

    /// Verifies interface LIST, POST, PATCH, and conditional DELETE requests sent through
    /// KubeRepository.
    /// Selector scoping, non-force creation, and idempotent 404 deletion protect detached parent
    /// ownership.
    #[tokio::test]
    async fn detached_interface_crud_uses_scoped_and_conditional_requests() {
        let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
        let repository = KubeRepository::with_client(Client::new(service, "default"));

        // LIST must forward the owner selector instead of fetching the namespace population.
        let list = DpuServiceInterfaceRepository::list(&repository, "test-ns", Some("owner=id"));
        let inspect_list = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::GET);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuserviceinterfaces"
            );
            assert_eq!(request.uri().query(), Some("&labelSelector=owner%3Did"));
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "apiVersion": "svc.dpu.nvidia.com/v1alpha1",
                            "kind": "DPUServiceInterfaceList",
                            "metadata": {},
                            "items": [],
                        })
                        .to_string()
                        .into_bytes(),
                    ))
                    .unwrap(),
            );
        };
        let (listed, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(list, inspect_list)
        })
        .await
        .expect("interface list request must complete");
        assert!(listed.unwrap().is_empty());

        // POST uses ordinary create semantics and therefore carries no force query parameter.
        let interface = detached_interface();
        let expected_interface = serde_json::to_value(&interface).unwrap();
        let create = DpuServiceInterfaceRepository::create(&repository, &interface);
        let inspect_create = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::POST);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuserviceinterfaces"
            );
            assert!(
                request.uri().query().is_none_or(str::is_empty),
                "ordinary creation must not include query parameters"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                expected_interface
            );
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"code": 409}).to_string().into_bytes()))
                    .unwrap(),
            );
        };
        let (created, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(create, inspect_create)
        })
        .await
        .expect("interface create request must complete");
        assert!(matches!(
            created,
            Err(DpfError::KubeError(kube::Error::Api(error))) if error.code == 409
        ));

        // PATCH carries the observed revision fence and returns the resulting interface generation.
        let patch = json!([
            {"op": "replace", "path": "/metadata/resourceVersion", "value": "1"},
            {"op": "replace", "path": "/spec", "value": expected_interface["spec"]},
        ]);
        let update = DpuServiceInterfaceRepository::patch(
            &repository,
            "service-interface",
            "test-ns",
            patch.clone(),
        );
        let inspect_update = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::PATCH);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuserviceinterfaces/service-interface"
            );
            assert_eq!(
                request.headers().get(http::header::CONTENT_TYPE).unwrap(),
                "application/json-patch+json"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                patch
            );
            let mut response = expected_interface.clone();
            response["metadata"]["resourceVersion"] = json!("2");
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(response.to_string().into_bytes()))
                    .unwrap(),
            );
        };
        let (updated, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(update, inspect_update)
        })
        .await
        .expect("interface patch request must complete");
        assert_eq!(
            updated.unwrap().metadata.resource_version.as_deref(),
            Some("2")
        );

        // A conditional DELETE forwards both identities and treats confirmed absence as success.
        let preconditions = DpfDeletePreconditions {
            uid: "interface-uid".to_string(),
            resource_version: "7".to_string(),
        };
        let delete = DpuServiceInterfaceRepository::delete_if_current(
            &repository,
            "service-interface",
            "test-ns",
            &preconditions,
        );
        let inspect_delete = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::DELETE);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuserviceinterfaces/service-interface"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({"preconditions": {"resourceVersion": "7", "uid": "interface-uid"}})
            );
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"code": 404}).to_string().into_bytes()))
                    .unwrap(),
            );
        };
        let (deleted, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(delete, inspect_delete)
        })
        .await
        .expect("interface delete request must complete");
        assert!(deleted.is_ok());
    }

    /// Verifies chain LIST, POST, PATCH, and conditional DELETE requests sent through
    /// KubeRepository.
    /// A stale conditional delete must remain visible so callers can preserve and reobserve a
    /// replacement.
    #[tokio::test]
    async fn detached_chain_crud_uses_scoped_and_conditional_requests() {
        let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
        let repository = KubeRepository::with_client(Client::new(service, "default"));

        // LIST carries the same owner selector contract as detached interfaces.
        let list = DpuServiceChainRepository::list(&repository, "test-ns", Some("owner=id"));
        let inspect_list = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::GET);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuservicechains"
            );
            assert_eq!(request.uri().query(), Some("&labelSelector=owner%3Did"));
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "apiVersion": "svc.dpu.nvidia.com/v1alpha1",
                            "kind": "DPUServiceChainList",
                            "metadata": {},
                            "items": [],
                        })
                        .to_string()
                        .into_bytes(),
                    ))
                    .unwrap(),
            );
        };
        let (listed, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(list, inspect_list)
        })
        .await
        .expect("chain list request must complete");
        assert!(listed.unwrap().is_empty());

        // POST must serialize the complete generated chain without apply or force semantics.
        let chain = detached_chain();
        let expected_chain = serde_json::to_value(&chain).unwrap();
        let create = DpuServiceChainRepository::create(&repository, &chain);
        let inspect_create = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::POST);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuservicechains"
            );
            assert!(
                request.uri().query().is_none_or(str::is_empty),
                "ordinary creation must not include query parameters"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                expected_chain
            );
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"code": 409}).to_string().into_bytes()))
                    .unwrap(),
            );
        };
        let (created, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(create, inspect_create)
        })
        .await
        .expect("chain create request must complete");
        assert!(matches!(
            created,
            Err(DpfError::KubeError(kube::Error::Api(error))) if error.code == 409
        ));

        // PATCH forwards the complete replacement spec and exposes the returned resource version.
        let patch = json!([
            {"op": "replace", "path": "/metadata/resourceVersion", "value": "1"},
            {"op": "replace", "path": "/spec", "value": expected_chain["spec"]},
        ]);
        let update = DpuServiceChainRepository::patch(
            &repository,
            "service-chain",
            "test-ns",
            patch.clone(),
        );
        let inspect_update = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::PATCH);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuservicechains/service-chain"
            );
            assert_eq!(
                request.headers().get(http::header::CONTENT_TYPE).unwrap(),
                "application/json-patch+json"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                patch
            );
            let mut response = expected_chain.clone();
            response["metadata"]["resourceVersion"] = json!("2");
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(response.to_string().into_bytes()))
                    .unwrap(),
            );
        };
        let (updated, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(update, inspect_update)
        })
        .await
        .expect("chain patch request must complete");
        assert_eq!(
            updated.unwrap().metadata.resource_version.as_deref(),
            Some("2")
        );

        // A stale DELETE sends both preconditions and propagates the conflict unchanged.
        let preconditions = DpfDeletePreconditions {
            uid: "chain-uid".to_string(),
            resource_version: "9".to_string(),
        };
        let delete = DpuServiceChainRepository::delete_if_current(
            &repository,
            "service-chain",
            "test-ns",
            &preconditions,
        );
        let inspect_delete = async {
            let (request, send_response) = handle.next_request().await.unwrap();
            assert_eq!(request.method(), Method::DELETE);
            assert_eq!(
                request.uri().path(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuservicechains/service-chain"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({"preconditions": {"resourceVersion": "9", "uid": "chain-uid"}})
            );
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"code": 409}).to_string().into_bytes()))
                    .unwrap(),
            );
        };
        let (deleted, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(delete, inspect_delete)
        })
        .await
        .expect("chain delete request must complete");
        assert!(matches!(
            deleted,
            Err(DpfError::KubeError(kube::Error::Api(error))) if error.code == 409
        ));
    }

    /// Verifies conditional DPUService deletion sends both Kubernetes identity preconditions.
    /// A conflict must remain visible so the controller preserves a same-name replacement and
    /// retries.
    #[tokio::test]
    async fn conditional_dpu_service_delete_sends_complete_preconditions() {
        // Inject an in-process Kubernetes service so the repository's exact HTTP request is
        // observable.
        let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
        let repository = KubeRepository::with_client(Client::new(service, "default"));
        let preconditions = DpfDeletePreconditions {
            uid: "observed-uid".to_string(),
            resource_version: "17".to_string(),
        };

        // Start the conditional delete while the mock API server captures and answers its request.
        let delete = DpuServiceRepository::delete_if_current(
            &repository,
            "extension-service",
            "test-ns",
            &preconditions,
        );
        let inspect_request = async {
            let (request, send_response) = handle
                .next_request()
                .await
                .expect("conditional delete must send one Kubernetes request");

            // The repository must target the exact namespaced DPUService with a DELETE.
            assert_eq!(request.method(), Method::DELETE);
            assert_eq!(
                request.uri(),
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuservices/extension-service"
            );

            // Both identities must reach the wire so Kubernetes can reject stale observations.
            let body = request
                .into_body()
                .collect_bytes()
                .await
                .expect("collect Kubernetes request body");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body)
                    .expect("delete request body must be JSON"),
                json!({
                    "preconditions": {
                        "resourceVersion": "17",
                        "uid": "observed-uid",
                    },
                })
            );

            // Model the apiserver rejecting a stale identity after a same-name replacement.
            send_response.send_response(
                Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"code": 409}).to_string().into_bytes()))
                    .expect("build Kubernetes conflict response"),
            );
        };

        let (delete_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(delete, inspect_request)
        })
        .await
        .expect("conditional delete request must complete");

        // The adapter must propagate the conflict so its caller can refresh and retry.
        assert!(matches!(
            delete_result,
            Err(DpfError::KubeError(kube::Error::Api(error))) if error.code == 409
        ));
    }
}
