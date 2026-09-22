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
    ) -> Result<(), DpfError> {
        let api: Api<DPUService> = self.api(namespace);
        api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPUService> = self.api(namespace);
        api.delete(name, &Default::default()).await?;
        Ok(())
    }
}

#[async_trait]
impl DpuServiceChainRepository for KubeRepository {
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUServiceChain>, DpfError> {
        let api = self.api(namespace);
        Ok(api.get_opt(name).await?)
    }

    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceChain>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items)
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

    async fn list(&self, namespace: &str) -> Result<Vec<DPUServiceInterface>, DpfError> {
        let api = self.api(namespace);
        let list = api.list(&ListParams::default()).await?;
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

    async fn delete(&self, name: &str, namespace: &str) -> Result<(), DpfError> {
        let api: Api<DPUServiceInterface> = self.api(namespace);
        match api.delete(name, &Default::default()).await {
            Ok(_) => {}
            Err(kube::Error::Api(error)) if error.code == 404 => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
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
