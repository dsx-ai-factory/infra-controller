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

//! Tests for DPF SDK initialization resources and lookup behavior.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use kube::Resource;
use tokio::sync::Notify;

use crate::crds::bfbs_generated::BFB;
use crate::crds::bluefieldsoftwares_generated::BlueFieldSoftware;
use crate::crds::dpudeployments_generated::DPUDeployment;
use crate::crds::dpuflavors_generated::{DPUFlavor, DpuFlavorServiceReadinessGate};
use crate::crds::dpuflavortemplates_generated::DPUFlavorTemplate;
use crate::crds::dpus_generated::{DPU, DpuStatusPhase};
use crate::crds::dpuserviceconfigurations_generated::DPUServiceConfiguration;
use crate::crds::dpuserviceinterfaces_generated::DPUServiceInterface;
use crate::crds::dpuservicenads_generated::DPUServiceNAD;
use crate::crds::dpuservicetemplates_generated::DPUServiceTemplate;
use crate::error::DpfError;
use crate::repository::{
    BfbRepository, BlueFieldSoftwareRepository, DpfOperatorConfigRepository,
    DpuDeploymentRepository, DpuFlavorRepository, DpuFlavorTemplateRepository, DpuRepository,
    DpuServiceConfigurationRepository, DpuServiceInterfaceRepository, DpuServiceNADRepository,
    DpuServiceTemplateRepository, K8sConfigRepository,
};
use crate::sdk::ResourceLabeler;
use crate::types::*;

const TEST_NS: &str = "sdk-init-ns";

fn ns_key(ns: &str, name: &str) -> String {
    format!("{}/{}", ns, name)
}

fn resource_key<T: Resource>(r: &T) -> String {
    format!(
        "{}/{}",
        r.meta().namespace.as_deref().unwrap_or(""),
        r.meta().name.as_deref().unwrap_or("")
    )
}

/// Stores test resources in shared Arc-backed maps so every clone observes the same cluster state.
#[derive(Clone, Default)]
struct InitializationMock {
    bfbs: Arc<DashMap<String, BFB>>,
    bluefield_softwares: Arc<DashMap<String, BlueFieldSoftware>>,
    flavors: Arc<DashMap<String, DPUFlavor>>,
    flavor_templates: Arc<DashMap<String, DPUFlavorTemplate>>,
    dpus: Arc<DashMap<String, DPU>>,
    deployments: Arc<DashMap<String, DPUDeployment>>,
    service_templates: Arc<DashMap<String, DPUServiceTemplate>>,
    service_configs: Arc<DashMap<String, DPUServiceConfiguration>>,
    nads: Arc<DashMap<String, DPUServiceNAD>>,
    service_interfaces: Arc<DashMap<String, DPUServiceInterface>>,
    blocked_service_interface_deletes: Arc<DashMap<String, ()>>,
    deferred_service_interface_deletes: Arc<DashMap<String, ()>>,
    deferred_service_interface_applies: Arc<DashMap<String, ()>>, // Holds named applies.
    service_interface_delete_requests: Arc<DashMap<String, usize>>,
    service_interface_delete_started: Arc<Notify>,
    service_interface_delete_release: Arc<Notify>,
    service_interface_apply_started: Arc<Notify>, // Signals the held apply.
    configs: Arc<DashMap<String, BTreeMap<String, String>>>,
    secrets: Arc<DashMap<String, BTreeMap<String, Vec<u8>>>>,
}

/// Supplies deterministic deployment labels so scoped-interface tests verify isolation without
/// depending on production label construction.
#[derive(Clone, Copy)]
struct InitializationLabeler;

impl ResourceLabeler for InitializationLabeler {
    fn node_labels_for_deployment_type(
        &self,
        deployment_type: DpuDeploymentType,
    ) -> Result<BTreeMap<String, String>, DpfError> {
        // These synthetic test-only keys prove callers consume labeler output rather than relying
        // on a particular production deployment-label spelling.
        let deployment_label = match deployment_type {
            DpuDeploymentType::Bf3 => "test.nvidia.com/bf3",
            DpuDeploymentType::Bf3Gb200 => "test.nvidia.com/bf3gb200",
            DpuDeploymentType::Bf4Generic => "test.nvidia.com/bf4",
            DpuDeploymentType::Bf4Astra => "test.nvidia.com/astra",
        };
        Ok(BTreeMap::from([
            (
                "feature.node.kubernetes.io/dpu-enabled".to_string(),
                "true".to_string(),
            ),
            (deployment_label.to_string(), "true".to_string()),
        ]))
    }
}

/// Provides one selected PF and VF so initialization tests can verify that every generated
/// resource consumes the same normalized intercept topology.
fn configured_intercept_bridging() -> DpfInterceptBridging {
    DpfInterceptBridging::new(
        vec![
            DpfInterceptBridge::new(
                DpfInterfaceIdentity {
                    controller_id: 2,
                    pf_id: 3,
                    vf_id: None,
                },
                "br-pf3",
                "p-pf3",
            ),
            DpfInterceptBridge::new(
                DpfInterfaceIdentity {
                    controller_id: 2,
                    pf_id: 3,
                    vf_id: Some(4),
                },
                "br-vf4",
                "p-vf4",
            ),
        ],
        16,
    )
    .expect("configured intercept topology must be valid")
}

/// Provides an otherwise valid Astra configuration that isolates the scoping invariant.
fn unscoped_astra_config() -> InitDpfResourcesConfig {
    InitDpfResourcesConfig {
        bluefield_software: Some(BlueFieldSoftwareParams {
            os_iso: "http://example.com/astra.iso".to_string(),
            pldm_fw_bundle: Some(BTreeMap::from([(
                "pldmid001".to_string(),
                "http://example.com/astra.pldm".to_string(),
            )])),
        }),
        deployment_name: "astra-deployment".to_string(),
        deployment_type: DpuDeploymentType::Bf4Astra,
        ..Default::default()
    }
}

/// Establishes the exact topology-free scoped Astra state emitted by predecessor initialization so
/// migration tests exercise a real persisted starting point rather than a hand-built approximation.
async fn seed_pre_topology_scoped_astra(
    mock: &InitializationMock,
) -> crate::sdk::DpfSdk<InitializationMock, InitializationLabeler> {
    // Supply the runtime configuration required while rendering Astra's fixed xplane files.
    mock.configs.insert(
        ns_key(TEST_NS, "ra2.2-runtime"),
        BTreeMap::from([(
            "RA2.2-runtime.yaml".to_string(),
            "runtimeConfig:\n  roce: []\n".to_string(),
        )]),
    );

    // Apply the predecessor inventory through the public split-initialization path.
    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .with_labeler(InitializationLabeler)
        .build_without_resources()
        .await
        .unwrap();
    sdk.create_initialization_objects(&InitDpfResourcesConfig {
        deployment_scoped_service_interfaces: true,
        ..unscoped_astra_config()
    })
    .await
    .expect("topology-free scoped Astra initialization must establish the old state");

    sdk
}

/// Asserts every DPF initialization CR store is empty so both public paths prove no-write safety.
fn assert_no_initialization_crs(mock: &InitializationMock) {
    assert!(mock.bfbs.is_empty());
    assert!(mock.bluefield_softwares.is_empty());
    assert!(mock.flavors.is_empty());
    assert!(mock.deployments.is_empty());
    assert!(mock.service_templates.is_empty());
    assert!(mock.service_configs.is_empty());
    assert!(mock.nads.is_empty());
    assert!(mock.service_interfaces.is_empty());
}

#[async_trait]
impl BfbRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<BFB>, DpfError> {
        Ok(self.bfbs.get(&ns_key(ns, name)).map(|r| r.clone()))
    }
    async fn list(&self, ns: &str) -> Result<Vec<BFB>, DpfError> {
        let prefix = format!("{}/", ns);
        Ok(self
            .bfbs
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }
    async fn create(&self, bfb: &BFB) -> Result<BFB, DpfError> {
        use crate::crds::bfbs_generated::{BfbStatus, BfbStatusPhase};
        let mut bfb_with_status = bfb.clone();
        bfb_with_status.status = Some(BfbStatus {
            file_name: None,
            phase: BfbStatusPhase::Ready,
            versions: None,
            conditions: None,
            observed_generation: None,
        });
        self.bfbs
            .insert(resource_key(&bfb_with_status), bfb_with_status.clone());
        Ok(bfb_with_status)
    }
    async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
        self.bfbs.remove(&ns_key(ns, name));
        Ok(())
    }
}

#[async_trait]
impl BlueFieldSoftwareRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<BlueFieldSoftware>, DpfError> {
        Ok(self
            .bluefield_softwares
            .get(&ns_key(ns, name))
            .map(|r| r.clone()))
    }
    async fn list(&self, ns: &str) -> Result<Vec<BlueFieldSoftware>, DpfError> {
        let prefix = format!("{}/", ns);
        Ok(self
            .bluefield_softwares
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }
    async fn create(&self, bfs: &BlueFieldSoftware) -> Result<BlueFieldSoftware, DpfError> {
        self.bluefield_softwares
            .insert(resource_key(bfs), bfs.clone());
        Ok(bfs.clone())
    }
    async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
        self.bluefield_softwares.remove(&ns_key(ns, name));
        Ok(())
    }
}

#[async_trait]
impl DpuFlavorRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUFlavor>, DpfError> {
        Ok(self.flavors.get(&ns_key(ns, name)).map(|r| r.clone()))
    }
    async fn create(&self, f: &DPUFlavor) -> Result<DPUFlavor, DpfError> {
        self.flavors.insert(resource_key(f), f.clone());
        Ok(f.clone())
    }
}

#[async_trait]
impl DpuFlavorTemplateRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUFlavorTemplate>, DpfError> {
        Ok(self
            .flavor_templates
            .get(&ns_key(ns, name))
            .map(|r| r.clone()))
    }
    async fn create(&self, template: &DPUFlavorTemplate) -> Result<DPUFlavorTemplate, DpfError> {
        self.flavor_templates
            .insert(resource_key(template), template.clone());
        Ok(template.clone())
    }
}

#[async_trait]
impl DpuRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPU>, DpfError> {
        Ok(self.dpus.get(&ns_key(ns, name)).map(|dpu| dpu.clone()))
    }

    async fn list(&self, ns: &str, _label_selector: Option<&str>) -> Result<Vec<DPU>, DpfError> {
        let prefix = format!("{ns}/");
        Ok(self
            .dpus
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }

    async fn patch_status(
        &self,
        _name: &str,
        _ns: &str,
        _patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        Ok(())
    }

    async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
        self.dpus.remove(&ns_key(ns, name));
        Ok(())
    }

    async fn delete_if_uid(&self, name: &str, ns: &str, uid: &str) -> Result<(), DpfError> {
        let current_uid = self
            .dpus
            .get(&ns_key(ns, name))
            .map(|dpu| dpu.metadata.uid.clone())
            .ok_or_else(|| DpfError::not_found("DPU", name))?;
        if current_uid.as_deref() != Some(uid) {
            return Err(DpfError::InvalidState(format!(
                "DPU {name} no longer has UID {uid}"
            )));
        }
        DpuRepository::delete(self, name, ns).await
    }

    fn watch<F, Fut>(
        &self,
        _ns: &str,
        _label_selector: Option<&str>,
        _handler: F,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        F: Fn(Arc<DPU>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        futures::future::pending()
    }
}

#[async_trait]
impl DpuDeploymentRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUDeployment>, DpfError> {
        Ok(self.deployments.get(&ns_key(ns, name)).map(|r| r.clone()))
    }
    async fn list(&self, ns: &str) -> Result<Vec<DPUDeployment>, DpfError> {
        let prefix = format!("{}/", ns);
        Ok(self
            .deployments
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }
    async fn apply(&self, d: &DPUDeployment) -> Result<DPUDeployment, DpfError> {
        self.deployments.insert(resource_key(d), d.clone());
        Ok(d.clone())
    }
    async fn patch(&self, name: &str, ns: &str, patch: serde_json::Value) -> Result<(), DpfError> {
        if let Some(mut dep) = self.deployments.get_mut(&ns_key(ns, name))
            && let Some(bfb) = patch.pointer("/spec/dpus/bfb").and_then(|v| v.as_str())
        {
            dep.spec.dpus.bfb = Some(bfb.to_string());
        }
        Ok(())
    }
    async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
        self.deployments.remove(&ns_key(ns, name));
        Ok(())
    }
}

#[async_trait]
impl DpuServiceTemplateRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUServiceTemplate>, DpfError> {
        Ok(self
            .service_templates
            .get(&ns_key(ns, name))
            .map(|r| r.clone()))
    }
    async fn list(&self, ns: &str) -> Result<Vec<DPUServiceTemplate>, DpfError> {
        let prefix = format!("{}/", ns);
        Ok(self
            .service_templates
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }
    async fn apply(&self, t: &DPUServiceTemplate) -> Result<DPUServiceTemplate, DpfError> {
        self.service_templates.insert(resource_key(t), t.clone());
        Ok(t.clone())
    }
}

#[async_trait]
impl DpuServiceConfigurationRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUServiceConfiguration>, DpfError> {
        Ok(self
            .service_configs
            .get(&ns_key(ns, name))
            .map(|r| r.clone()))
    }
    async fn list(&self, ns: &str) -> Result<Vec<DPUServiceConfiguration>, DpfError> {
        let prefix = format!("{}/", ns);
        Ok(self
            .service_configs
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }
    async fn apply(
        &self,
        c: &DPUServiceConfiguration,
    ) -> Result<DPUServiceConfiguration, DpfError> {
        self.service_configs.insert(resource_key(c), c.clone());
        Ok(c.clone())
    }
}

#[async_trait]
impl DpuServiceNADRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUServiceNAD>, DpfError> {
        Ok(self.nads.get(&ns_key(ns, name)).map(|r| r.clone()))
    }
    async fn list(&self, ns: &str) -> Result<Vec<DPUServiceNAD>, DpfError> {
        let prefix = format!("{}/", ns);
        Ok(self
            .nads
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }
    async fn apply(&self, nad: &DPUServiceNAD) -> Result<DPUServiceNAD, DpfError> {
        self.nads.insert(resource_key(nad), nad.clone());
        Ok(nad.clone())
    }
}

#[async_trait]
impl DpuServiceInterfaceRepository for InitializationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUServiceInterface>, DpfError> {
        Ok(self
            .service_interfaces
            .get(&ns_key(ns, name))
            .map(|r| r.clone()))
    }
    async fn list(&self, ns: &str) -> Result<Vec<DPUServiceInterface>, DpfError> {
        let prefix = format!("{}/", ns);
        Ok(self
            .service_interfaces
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect())
    }
    async fn apply(&self, iface: &DPUServiceInterface) -> Result<DPUServiceInterface, DpfError> {
        let key = resource_key(iface);
        // A canceled task leaves earlier applies persisted while this selected replacement remains
        // absent, matching an interrupted sequence of Kubernetes requests.
        if self.deferred_service_interface_applies.contains_key(&key) {
            self.service_interface_apply_started.notify_one();
            std::future::pending::<()>().await;
        }

        self.service_interfaces.insert(key, iface.clone());
        Ok(iface.clone())
    }

    async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
        let key = ns_key(ns, name);
        self.service_interface_delete_requests
            .entry(key.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        if self.deferred_service_interface_deletes.contains_key(&key) {
            self.service_interface_delete_started.notify_one();
            return Ok(());
        }
        if self.blocked_service_interface_deletes.contains_key(&key) {
            self.service_interface_delete_started.notify_one();
            self.service_interface_delete_release.notified().await;
        }
        self.service_interfaces.remove(&key);
        Ok(())
    }
}

#[async_trait]
impl K8sConfigRepository for InitializationMock {
    /// Create-only, like the real repository: an existing ConfigMap is reported
    /// back rather than overwritten.
    async fn create_configmap(
        &self,
        name: &str,
        ns: &str,
        data: BTreeMap<String, String>,
    ) -> Result<bool, DpfError> {
        match self.configs.entry(ns_key(ns, name)) {
            dashmap::mapref::entry::Entry::Occupied(_) => Ok(false),
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(data);
                Ok(true)
            }
        }
    }

    async fn get_configmap(
        &self,
        name: &str,
        ns: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DpfError> {
        Ok(self.configs.get(&ns_key(ns, name)).map(|r| r.clone()))
    }
    async fn apply_configmap(
        &self,
        name: &str,
        ns: &str,
        data: BTreeMap<String, String>,
    ) -> Result<(), DpfError> {
        self.configs.insert(ns_key(ns, name), data);
        Ok(())
    }
    async fn get_secret(
        &self,
        name: &str,
        ns: &str,
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, DpfError> {
        Ok(self.secrets.get(&ns_key(ns, name)).map(|r| r.clone()))
    }
    async fn apply_secret(
        &self,
        name: &str,
        ns: &str,
        data: BTreeMap<String, Vec<u8>>,
    ) -> Result<(), DpfError> {
        self.secrets.insert(ns_key(ns, name), data);
        Ok(())
    }
}

#[async_trait]
impl DpfOperatorConfigRepository for InitializationMock {
    async fn get(
        &self,
        _name: &str,
        _ns: &str,
    ) -> Result<Option<crate::crds::dpfoperatorconfigs_generated::DPFOperatorConfig>, DpfError>
    {
        Ok(None)
    }

    async fn patch(&self, _: &str, _: &str, _: serde_json::Value) -> Result<(), DpfError> {
        Ok(())
    }
}

/// A conditional delete must preserve a replacement DPU whose UID differs
/// from the stale object observed by the migration reconciler.
#[tokio::test]
async fn conditional_dpu_delete_rejects_uid_mismatch() {
    let mock = InitializationMock::default();
    let dpu_name = "node-host-device-dpu";
    let mut replacement = super::helpers::make_dpu(
        TEST_NS,
        dpu_name,
        "device-dpu",
        "node-host",
        DpuStatusPhase::Ready,
    );
    replacement.metadata.uid = Some("replacement-uid".to_string());
    mock.dpus.insert(resource_key(&replacement), replacement);

    let error = DpuRepository::delete_if_uid(&mock, dpu_name, TEST_NS, "stale-uid")
        .await
        .expect_err("a stale UID must not delete the replacement DPU");

    assert!(matches!(error, DpfError::InvalidState(_)));
    assert!(
        DpuRepository::get(&mock, dpu_name, TEST_NS)
            .await
            .unwrap()
            .is_some(),
        "the replacement DPU must remain after the rejected delete"
    );
}

#[tokio::test]
async fn test_create_initialization_objects() {
    let mock = InitializationMock::default();

    let config = InitDpfResourcesConfigBuilder::default()
        .bfb_url("http://example.com/test.bfb")
        .build()
        .expect("BF3 initialization test configuration must be valid");
    let deployment_name = config.deployment_name.clone();

    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .initialize(&config)
        .await
        .unwrap();

    let bfbs = BfbRepository::list(&mock, TEST_NS).await.unwrap();
    assert_eq!(bfbs.len(), 1);

    let expected_flavor_name = crate::flavor::default_flavor(TEST_NS, &config.proxy)
        .unwrap()
        .unique_name(crate::flavor::DEFAULT_FLAVOR_NAME)
        .unwrap();
    let flavor = DpuFlavorRepository::get(&mock, &expected_flavor_name, TEST_NS)
        .await
        .unwrap();
    assert!(flavor.is_some());

    let deployment = DpuDeploymentRepository::get(&mock, &deployment_name, TEST_NS)
        .await
        .unwrap();
    assert!(deployment.is_some());

    // The default migration mode preserves legacy names and the absent node selector.
    let p0 = DpuServiceInterfaceRepository::get(&mock, "p0", TEST_NS)
        .await
        .unwrap()
        .expect("legacy p0 ServiceInterface must exist");
    assert!(p0.spec.template.spec.node_selector.is_none());
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "p0-bf3", TEST_NS)
            .await
            .unwrap()
            .is_none()
    );

    assert!(
        DpuServiceInterfaceRepository::get(&mock, "pf1hpf", TEST_NS)
            .await
            .unwrap()
            .is_none(),
        "BF3 must not create a ServiceInterface for the hidden host PF1"
    );
    let deployment = deployment.unwrap();
    assert!(
        deployment
            .spec
            .service_chains
            .unwrap()
            .switches
            .iter()
            .all(|switch| {
                switch.ports.iter().all(|port| {
                    port.service_interface.as_ref().is_none_or(|interface| {
                        interface
                            .match_labels
                            .get("interface")
                            .is_none_or(|name| name != "pf1hpf")
                    })
                })
            })
    );

    let secret = K8sConfigRepository::get_secret(&mock, "bmc-shared-password", TEST_NS)
        .await
        .unwrap();
    assert!(secret.is_some());

    drop(sdk);
}

/// Verifies SF overflow fails before the builder writes its BMC Secret or any DPF CR because
/// invalid capacity must not leave a partially initialized DPF namespace.
#[tokio::test]
async fn sf_overflow_fails_before_initialization_writes() {
    // Use a valid configured inventory so the SF sum fails only at the arithmetic boundary.
    let mock = InitializationMock::default();
    let config = InitDpfResourcesConfig {
        intercept_bridging: Some(configured_intercept_bridging()),
        pf_total_sf_reserved: u32::MAX,
        ..Default::default()
    };

    // Preflight must reject the configuration before the first initialization write.
    let result = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .initialize(&config)
        .await;
    assert!(matches!(result, Err(DpfError::ConfigError(_))));
    assert!(mock.secrets.is_empty());
    assert_no_initialization_crs(&mock);
}

/// Verifies one-shot Astra initialization rejects global interfaces before its first write.
/// Qualified local SF references must also fail before writes so SDK namespace forwarding
/// cannot bypass the direct SF-consumer boundary.
#[tokio::test]
async fn unscoped_astra_builder_initialization_writes_nothing() {
    // Target namespace qualification is the variable; the existing fixture supplies Astra defaults.
    let mut qualified = unscoped_astra_config();
    qualified.deployment_scoped_service_interfaces = true; // Pass the earlier scoping check.
    qualified.services.push(ServiceDefinition {
        interfaces: vec![ServiceInterface {
            name: "direct_dhcp_if".to_string(), // Absent from effective service chains.
            network: format!("{TEST_NS}/mybrsfc-dhcp-bf4astra"), // Names the local rendered NAD.
        }],
        service_nads: vec![ServiceNAD {
            name: "mybrsfc-dhcp".to_string(), // Emitted with the Astra deployment suffix.
            bridge: Some("br-sfc".to_string()),
            resource_type: ServiceNADResourceType::Sf, // Makes the unchained consumer unsupported.
            ipam: Some(false),
            mtu: Some(1500),
        }],
        ..ServiceDefinition::new(DHCP_SERVER_SERVICE_NAME, "repo", "chart", "1")
    });

    for (config, expected_error) in [
        // Retain protection against global interfaces binding Astra nodes.
        (
            unscoped_astra_config(),
            "BF4 Astra requires deployment_scoped_service_interfaces=true",
        ),
        // The SDK must supply its namespace before committing even the shared Secret.
        (
            qualified,
            "BF4 Astra does not support direct SF-NAD consumers outside service chains",
        ),
    ] {
        // Exercise the public initializer with a fresh repository for each failure boundary.
        let mock = InitializationMock::default();
        let result =
            crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
                .with_labeler(InitializationLabeler)
                .initialize(&config)
                .await;

        // Require the intended preflight failure and prove it precedes all initialization writes.
        let Err(DpfError::ConfigError(message)) = result else {
            panic!("invalid Astra initialization must fail during preflight");
        };
        assert_eq!(message, expected_error);
        assert!(mock.secrets.is_empty());
        assert_no_initialization_crs(&mock);
    }
}

/// Verifies split-phase Astra initialization preserves its existing Secret and writes no CRs.
#[tokio::test]
async fn unscoped_astra_split_initialization_writes_no_resources() {
    // Build the SDK first, establishing the split-phase path's expected Secret baseline.
    let mock = InitializationMock::default();
    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .with_labeler(InitializationLabeler)
        .build_without_resources()
        .await
        .unwrap();
    let secret_before = K8sConfigRepository::get_secret(&mock, "bmc-shared-password", TEST_NS)
        .await
        .unwrap()
        .expect("split-phase SDK construction must write its shared Secret");

    // Supplying unsafe Astra configuration must perform no subsequent initialization write.
    let error = sdk
        .create_initialization_objects(&unscoped_astra_config())
        .await
        .expect_err("unscoped Astra initialization must fail");
    assert!(matches!(error, DpfError::ConfigError(_)));
    assert_eq!(
        K8sConfigRepository::get_secret(&mock, "bmc-shared-password", TEST_NS)
            .await
            .unwrap()
            .expect("scoping rejection must preserve the shared Secret"),
        secret_before
    );
    assert_no_initialization_crs(&mock);
}

/// Verifies public initialization persists aligned HBN configuration and chain references.
/// BF3's legacy BFB path and generic BF4's scoped software path must both retain slot wiring.
/// Complete five-slot DHCP attachments must retain their bridges and MTU while ordinary DHCP/FMDS
/// NADs keep their existing rendering and deployment-specific references; the flavor must render the configured pool.
#[tokio::test]
async fn service_vpc_initialization_keeps_hbn_configuration_and_chain_aligned() {
    for (deployment_type, sf_pool, sf_bar_size, resource_suffix, interface_suffix) in [
        // Legacy BF3 keeps unsuffixed references and the BFB path with 27 base SFs.
        (DpuDeploymentType::Bf3, 42, 10, "", ""),
        // Generic BF4 keeps scoped references and the software path with 28 base SFs.
        (DpuDeploymentType::Bf4Generic, 43, 14, "-bf4generic", "-bf4"),
    ] {
        let mock = InitializationMock::default();
        // Five slots and five endpoint reservations complete each platform's base inventory.
        let slots = crate::ServiceVpcSlots::new(5).expect("valid five-slot inventory");
        let interfaces = crate::build_deployment_dpu_interfaces(
            deployment_type,
            crate::DEFAULT_DPU_NUM_OF_VFS,
            None,
        );
        // Retain the ordinary chained services so multiple NAD rendering also proves startup compatibility.
        let chained_service = |name: &str, network: &str| ServiceDefinition {
            interfaces: interfaces
                .iter()
                .flat_map(|interface| interface.chained_svc_if.iter().flatten())
                .filter(|(service, _)| service == name)
                .map(|(_, name)| ServiceInterface {
                    name: name.clone(),
                    network: network.to_string(),
                })
                .collect(),
            service_nads: if name == DOCA_HBN_SERVICE_NAME {
                Vec::new()
            } else {
                vec![ServiceNAD {
                    name: network.to_string(),
                    bridge: Some("br-sfc".to_string()),
                    resource_type: ServiceNADResourceType::Sf,
                    ipam: Some(false),
                    mtu: Some(1500),
                }]
            },
            ..ServiceDefinition::new(name, "repo", "chart", "1")
        };
        let mut hbn = chained_service(DOCA_HBN_SERVICE_NAME, DOCA_HBN_SERVICE_NETWORK);
        slots.append_hbn_interfaces(&mut hbn.interfaces);
        let mut dhcp = chained_service(DHCP_SERVER_SERVICE_NAME, "mybrsfc-dhcp");
        slots.append_dhcp_interfaces(&mut dhcp);
        let fmds = chained_service(FMDS_SERVICE_NAME, "mybrsfc-fmds");
        let builder = InitDpfResourcesConfigBuilder::default()
            .deployment_type(deployment_type)
            .services(vec![hbn, dhcp, fmds])
            .service_vpc_slots(slots)
            .max_active_service_vpc_interfaces_per_dpu(5)
            .pf_total_sf_reserved(sf_pool)
            .deployment_scoped_service_interfaces(!interface_suffix.is_empty())
            .interfaces(interfaces);
        // Follow each platform's provisioning source so the BF3 case exercises the BFB path.
        let config = if deployment_type == DpuDeploymentType::Bf3 {
            builder.bfb_url("http://example.com/bf3.bfb")
        } else {
            builder.bluefield_software(BlueFieldSoftwareParams {
                os_iso: "http://example.com/bf4.iso".to_string(),
                pldm_fw_bundle: Some(BTreeMap::from([(
                    "psid".to_string(),
                    "http://example.com/fw.pldm".to_string(),
                )])),
            })
        }
        .build()
        .expect("service-VPC initialization test configuration must be valid");

        // Apply the complete inventory through the public SDK initializer.
        // Startup's split initialization shares the same resource creation implementation.
        crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
            .with_labeler(InitializationLabeler)
            .initialize(&config)
            .await
            .unwrap();

        // Read persisted CRs rather than trusting the input definitions or apply return values.
        let hbn_config = DpuServiceConfigurationRepository::get(
            &mock,
            &format!("{DOCA_HBN_SERVICE_NAME}{resource_suffix}"),
            TEST_NS,
        )
        .await
        .unwrap()
        .expect("HBN service configuration must exist");
        let hbn_interfaces = hbn_config.spec.interfaces.unwrap();
        assert!(hbn_interfaces.iter().any(|interface| {
            interface.name == "iface_svc_0" && interface.network == DOCA_HBN_SERVICE_NETWORK
        }));

        let deployment = DpuDeploymentRepository::get(&mock, "dpu-deployment", TEST_NS)
            .await
            .unwrap()
            .expect("DPU deployment must exist");
        let slot_switch = deployment
            .spec
            .service_chains
            .unwrap()
            .switches
            .into_iter()
            .find(|switch| {
                switch.ports.iter().any(|port| {
                    port.service
                        .as_ref()
                        .is_some_and(|service| service.interface == "iface_svc_0")
                })
            })
            .expect("service-VPC chain must reference the HBN interface");
        assert!(slot_switch.ports.iter().any(|port| {
            port.service_interface.as_ref().is_some_and(|interface| {
                interface.match_labels.get("interface").map(String::as_str)
                    == Some("service-vpc-slot-0")
            })
        }));

        // DHCP consumes exactly one direct SF per slot and references each slot's own NAD.
        let dhcp_config = DpuServiceConfigurationRepository::get(
            &mock,
            &format!("{DHCP_SERVER_SERVICE_NAME}{resource_suffix}"),
            TEST_NS,
        )
        .await
        .expect("DHCP configuration lookup succeeds")
        .expect("DHCP configuration exists");
        let listeners = dhcp_config.spec.interfaces.expect("DHCP listeners exist");
        assert_eq!(
            listeners
                .iter()
                .filter(|listener| listener.name.starts_with("d_iface_svc_"))
                .count(),
            5
        );
        assert_eq!(mock.nads.len(), 7);
        let switches = DpuDeploymentRepository::get(&mock, "dpu-deployment", TEST_NS)
            .await
            .expect("deployment lookup succeeds")
            .expect("deployment exists")
            .spec
            .service_chains
            .expect("chains exist")
            .switches;
        for (index, listener) in listeners
            .iter()
            .filter(|listener| listener.name.starts_with("d_iface_svc_"))
            .enumerate()
        {
            assert_eq!(listener.name, format!("d_iface_svc_{index}"));
            assert!(listener.name.len() <= 15);
            assert_eq!(
                listener.network,
                format!("service-vpc-dhcp-slot{index}{resource_suffix}")
            );
            let nad = DpuServiceNADRepository::get(&mock, &listener.network, TEST_NS)
                .await
                .expect("slot NAD lookup succeeds")
                .expect("slot NAD exists");
            assert_eq!(
                nad.spec.bridge.as_deref(),
                Some(format!("br-svc-{index}").as_str())
            );
            assert_eq!(nad.spec.ipam, Some(false));
            assert_eq!(nad.spec.service_mtu, Some(crate::SERVICE_VPC_MTU));
            assert!(matches!(
                nad.spec.resource_type,
                crate::crds::dpuservicenads_generated::DpuServiceNadResourceType::Sf
            ));
            let patch = DpuServiceInterfaceRepository::get(
                &mock,
                &format!("service-vpc-slot-{index}{interface_suffix}"),
                TEST_NS,
            )
            .await
            .expect("patch lookup succeeds")
            .expect("slot patch exists");
            // Legacy BF3 keeps selector-free parents; generic BF4 selects its deployment's DPU nodes.
            let node_selector = patch.spec.template.spec.node_selector;
            assert_eq!(node_selector.is_some(), !interface_suffix.is_empty());
            assert_eq!(
                node_selector.and_then(|selector| selector.match_labels),
                (!interface_suffix.is_empty()).then(|| BTreeMap::from([(
                    "svc.dpu.nvidia.com/owned-by-dpudeployment".to_string(),
                    format!("{TEST_NS}_dpu-deployment")
                )]))
            );
            assert_eq!(
                patch
                    .spec
                    .template
                    .spec
                    .template
                    .spec
                    .patch
                    .expect("patch endpoint")
                    .peer_bridge,
                format!("br-svc-{index}")
            );
            assert!(
                switches
                    .iter()
                    .any(|switch| switch.ports.iter().any(|port| {
                        port.service.as_ref().is_some_and(|service| {
                            service.name == DOCA_HBN_SERVICE_NAME
                                && service.interface == format!("iface_svc_{index}")
                        })
                    }))
            );
        }

        // Ordinary service NADs retain their bridge, resource type, IPAM and MTU after the multi-NAD change.
        for (service, network) in [
            // DHCP's ordinary listeners keep their deployment-specific br-sfc SF network beside the new slot NADs.
            (DHCP_SERVER_SERVICE_NAME, "mybrsfc-dhcp"),
            // FMDS remains a separate ordinary br-sfc SF consumer with the same rendering contract.
            (FMDS_SERVICE_NAME, "mybrsfc-fmds"),
        ] {
            let network = format!("{network}{resource_suffix}");
            let nad = DpuServiceNADRepository::get(&mock, &network, TEST_NS)
                .await
                .expect("ordinary NAD lookup succeeds")
                .expect("ordinary NAD exists");
            assert_eq!(nad.spec.bridge.as_deref(), Some("br-sfc"));
            assert_eq!(nad.spec.ipam, Some(false));
            assert_eq!(nad.spec.service_mtu, Some(1500));
            assert!(matches!(
                nad.spec.resource_type,
                crate::crds::dpuservicenads_generated::DpuServiceNadResourceType::Sf
            ));
            let service_config = DpuServiceConfigurationRepository::get(
                &mock,
                &format!("{service}{resource_suffix}"),
                TEST_NS,
            )
            .await
            .expect("ordinary service configuration lookup succeeds")
            .expect("ordinary service configuration exists");
            let ordinary_interfaces = service_config
                .spec
                .interfaces
                .expect("ordinary service interfaces exist")
                .into_iter()
                .filter(|interface| !interface.name.starts_with("d_iface_svc_"))
                .collect::<Vec<_>>();
            assert!(!ordinary_interfaces.is_empty());
            assert!(
                ordinary_interfaces
                    .iter()
                    .all(|interface| interface.network == network)
            );
        }

        // The persisted flavor renders each platform's configured SF pool and bootstraps all five isolated bridges.
        let flavor_name = deployment
            .spec
            .dpus
            .flavor
            .expect("deployment flavor reference");
        let flavor = DpuFlavorRepository::get(&mock, &flavor_name, TEST_NS)
            .await
            .expect("flavor lookup succeeds")
            .expect("flavor exists");
        let parameters = flavor.spec.nvconfig.expect("NVConfig exists")[0]
            .parameters
            .clone()
            .expect("NVConfig parameters");
        assert!(parameters.contains(&format!("PF_TOTAL_SF={sf_pool}")));
        assert!(parameters.contains(&format!("PF_SF_BAR_SIZE={sf_bar_size}")));
        let ovs = flavor
            .spec
            .ovs
            .expect("OVS configuration")
            .raw_config_script
            .expect("OVS bootstrap");
        for index in 0..5 {
            assert!(ovs.contains(&format!("_ovs-vsctl --may-exist add-br br-svc-{index}")));
        }
        assert!(flavor.spec.dpu_resources.is_none());
        assert!(flavor.spec.system_reserved_resources.is_none());
    }
}

/// Verifies partial HBN inventory fails preflight so DHCP completeness cannot conceal a missing HBN SF.
#[test]
fn service_vpc_config_rejects_missing_hbn_interface() {
    // Keep every base HBN endpoint but omit the slot interface; DHCP is complete.
    let slots = crate::ServiceVpcSlots::new(1).expect("valid slot count");
    let interfaces = crate::build_deployment_dpu_interfaces(
        DpuDeploymentType::Bf3,
        crate::DEFAULT_DPU_NUM_OF_VFS,
        None,
    );
    let hbn = ServiceDefinition {
        interfaces: interfaces
            .iter()
            .flat_map(|interface| interface.chained_svc_if.iter().flatten())
            .filter(|(service, _)| service == DOCA_HBN_SERVICE_NAME)
            .map(|(_, name)| ServiceInterface {
                name: name.clone(),
                network: DOCA_HBN_SERVICE_NETWORK.to_string(),
            })
            .collect(),
        ..ServiceDefinition::new(DOCA_HBN_SERVICE_NAME, "repo", "chart", "1")
    };
    let mut dhcp = ServiceDefinition::new(DHCP_SERVER_SERVICE_NAME, "repo", "chart", "1");
    slots.append_dhcp_interfaces(&mut dhcp);
    let error = InitDpfResourcesConfigBuilder::default()
        .services(vec![hbn, dhcp])
        .interfaces(interfaces)
        .service_vpc_slots(slots)
        .build()
        .expect_err("missing HBN slot interface");

    // Require the HBN error so a missing-DHCP or capacity error cannot satisfy this proof.
    assert!(
        matches!(&error, DpfError::ConfigError(message)
            if message == "doca-hbn interface inventory must exactly match the resolved DPF HBN chains"),
        "{error}"
    );
}

#[test]
fn config_rejects_sf_overflow() {
    let result = InitDpfResourcesConfigBuilder::default()
        .intercept_bridging(configured_intercept_bridging())
        .pf_total_sf_reserved(u32::MAX)
        .build();
    assert!(matches!(result, Err(DpfError::ConfigError(_))));
}

#[test]
fn config_rejects_unscoped_astra() {
    let result = InitDpfResourcesConfigBuilder::default()
        .bluefield_software(BlueFieldSoftwareParams {
            os_iso: "http://example.com/astra.iso".to_string(),
            pldm_fw_bundle: Some(BTreeMap::from([(
                "pldmid001".to_string(),
                "http://example.com/astra.pldm".to_string(),
            )])),
        })
        .deployment_name("astra-deployment")
        .deployment_type(DpuDeploymentType::Bf4Astra)
        .build();

    let Err(error) = result else {
        panic!("unscoped Astra initialization must fail");
    };
    assert!(matches!(&error, DpfError::ConfigError(_)));
    assert!(
        error
            .to_string()
            .contains("BF4 Astra requires deployment_scoped_service_interfaces=true")
    );
}

#[tokio::test]
async fn test_create_initialization_objects_bluefield_software() {
    let mock = InitializationMock::default();

    let config = InitDpfResourcesConfigBuilder::default()
        .bluefield_software(BlueFieldSoftwareParams {
            os_iso: "http://example.com/os.iso".to_string(),
            pldm_fw_bundle: Some(BTreeMap::from([(
                "pldmid001".to_string(),
                "http://example.com/astra.pldm".to_string(),
            )])),
        })
        .deployment_name("bf4-dep")
        .deployment_type(DpuDeploymentType::Bf4Generic)
        .build()
        .expect("BF4 initialization test configuration must be valid");

    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .initialize(&config)
        .await
        .unwrap();

    // A BlueFieldSoftware CR is created; no BFB is.
    let bfbs = BfbRepository::list(&mock, TEST_NS).await.unwrap();
    assert!(
        bfbs.is_empty(),
        "no BFB should be created for a BF4 deployment"
    );
    let bfsw = BlueFieldSoftwareRepository::list(&mock, TEST_NS)
        .await
        .unwrap();
    assert_eq!(bfsw.len(), 1);
    assert_eq!(bfsw[0].spec.os_iso, "http://example.com/os.iso");
    assert_eq!(
        bfsw[0].spec.pldm_fw_bundle,
        Some(serde_json::json!({
            "pldmid001": "http://example.com/astra.pldm"
        }))
    );

    // The DPUDeployment references the BlueFieldSoftware CR, not a BFB.
    let deployment = DpuDeploymentRepository::get(&mock, "bf4-dep", TEST_NS)
        .await
        .unwrap()
        .expect("bf4 deployment created");
    assert_eq!(
        deployment.spec.dpus.blue_field_software.as_deref(),
        Some(bfsw[0].metadata.name.as_deref().unwrap())
    );
    assert!(deployment.spec.dpus.bfb.is_none());

    drop(sdk);
}

/// Verifies every DPF profile consumes one intercept topology while retaining isolated flavors,
/// interfaces, chains, and selectors so Astra's fixed xplane resources cannot cross-bind.
#[tokio::test]
async fn scoped_bf3_gb200_bf4_and_astra_initialization_coexists() {
    // Build one SDK so all four deployment classes share the production namespace.
    let mock = InitializationMock::default();
    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .with_labeler(InitializationLabeler)
        .build_without_resources()
        .await
        .unwrap();

    // Supply every mandatory logical service so deployment references mirror production setup.
    let services = [
        DTS_SERVICE_NAME,
        DOCA_HBN_SERVICE_NAME,
        DPU_AGENT_SERVICE_NAME,
        DHCP_SERVER_SERVICE_NAME,
        FMDS_SERVICE_NAME,
        OTEL_COLLECTOR_SERVICE_NAME,
    ]
    .into_iter()
    .map(|name| ServiceDefinition::new(name, "repo", "chart", "1.0.0"))
    .collect::<Vec<_>>();
    let topology = configured_intercept_bridging();
    let configs = [
        // BF3 proves the configured topology through the BFB flavor path.
        InitDpfResourcesConfigBuilder::default()
            .bfb_url("http://example.com/bf3.bfb")
            .deployment_name("bf3-deployment")
            .flavor_name("bf3-flavor")
            .services(services.clone())
            .deployment_scoped_service_interfaces(true)
            .intercept_bridging(topology.clone())
            .deployment_type(DpuDeploymentType::Bf3)
            .build()
            .expect("scoped BF3 test configuration must be valid"),
        // GB200 BF3 reuses the BF3 source and services but owns a distinct flavor and selector.
        InitDpfResourcesConfigBuilder::default()
            .bfb_url("http://example.com/bf3.bfb")
            .deployment_name("bf3-gb200-deployment")
            .flavor_name("bf3-gb200-flavor")
            .services(services.clone())
            .deployment_scoped_service_interfaces(true)
            .intercept_bridging(topology.clone())
            .deployment_type(DpuDeploymentType::Bf3Gb200)
            .build()
            .expect("scoped GB200 test configuration must be valid"),
        // Generic BF4 proves the same topology through its BlueFieldSoftware path.
        InitDpfResourcesConfigBuilder::default()
            .bluefield_software(BlueFieldSoftwareParams {
                os_iso: "http://example.com/bf4.iso".to_string(),
                pldm_fw_bundle: Some(BTreeMap::from([(
                    "pldmid001".to_string(),
                    "http://example.com/bf4.pldm".to_string(),
                )])),
            })
            .deployment_name("bf4-deployment")
            .flavor_name("bf4-flavor")
            .services(services.clone())
            .deployment_scoped_service_interfaces(true)
            .intercept_bridging(topology.clone())
            .deployment_type(DpuDeploymentType::Bf4Generic)
            .build()
            .expect("scoped generic BF4 test configuration must be valid"),
        // Astra proves the shared PF/VF topology composes with its fixed CX/xplane interfaces.
        InitDpfResourcesConfigBuilder::default()
            .bluefield_software(BlueFieldSoftwareParams {
                os_iso: "http://example.com/astra.iso".to_string(),
                pldm_fw_bundle: Some(BTreeMap::from([(
                    "pldmid001".to_string(),
                    "http://example.com/astra.pldm".to_string(),
                )])),
            })
            .deployment_name("astra-deployment")
            .flavor_name("astra-flavor")
            .services(services)
            .deployment_scoped_service_interfaces(true)
            .intercept_bridging(topology)
            .enable_delay_host_init(true)
            .deployment_type(DpuDeploymentType::Bf4Astra)
            .build()
            .expect("scoped Astra test configuration must be valid"),
    ];

    // Provide the Astra runtime ConfigMap required while rendering its fixed xplane files.
    mock.configs.insert(
        ns_key(TEST_NS, "ra2.2-runtime"),
        BTreeMap::from([(
            "RA2.2-runtime.yaml".to_string(),
            "runtimeConfig:\n  roce: []\n".to_string(),
        )]),
    );

    // Apply every class through the public split-initialization path used by multi-deployment setup.
    for config in &configs {
        sdk.create_initialization_objects(config).await.unwrap();
    }

    // All immutable flavors/templates and deployments must coexist without overwrite.
    assert_eq!(
        DpuDeploymentRepository::list(&mock, TEST_NS)
            .await
            .unwrap()
            .len(),
        4
    );
    assert_eq!(mock.flavors.len(), 3);
    assert_eq!(mock.flavor_templates.len(), 1);

    // The effective inventories must produce exact, non-overlapping scoped resource names.
    let interfaces = DpuServiceInterfaceRepository::list(&mock, TEST_NS)
        .await
        .unwrap();
    let interface_names = interfaces
        .iter()
        .map(|interface| interface.metadata.name.clone().unwrap())
        .collect::<BTreeSet<_>>();
    let mut expected_interface_names = BTreeSet::new();
    for suffix in ["bf3", "bf3gb200", "bf4", "astra"] {
        for logical_name in ["p0", "p1", "c2pf3", "c2pf3vf4"] {
            expected_interface_names.insert(format!("{logical_name}-{suffix}"));
        }
    }
    // Astra retains its fixed xplane resources around the shared replacement PF/VF inventory.
    let astra_chainable_logical_names = ["p0", "p1", "c2pf3", "c2pf3vf4"]
        .into_iter()
        .map(|name| name.to_string())
        .collect::<BTreeSet<_>>();
    let mut astra_logical_names = astra_chainable_logical_names.clone();
    let astra_xplane_group_ids = [
        "r0swpln0", "r1swpln0", "r0swpln1", "r1swpln1", "r2swpln0", "r3swpln0", "r2swpln1",
        "r3swpln1",
    ];
    astra_logical_names.extend(astra_xplane_group_ids.into_iter().flat_map(|group_id| {
        [
            format!("p-brcx-{group_id}-to-br-sfc"),
            format!("p-br-xplane-{group_id}-to-br-sfc"),
        ]
    }));
    expected_interface_names.extend(
        astra_logical_names
            .iter()
            .map(|logical_name| format!("{logical_name}-astra")),
    );
    assert_eq!(interface_names, expected_interface_names);

    // Each interface group must select the remote DPU Node by DPF deployment ownership. The
    // management-plane class labels used by DPUNode selectors do not exist in the DPU cluster.
    for (suffix, deployment_name) in [
        ("bf3", "bf3-deployment"),
        ("bf3gb200", "bf3-gb200-deployment"),
        ("bf4", "bf4-deployment"),
        ("astra", "astra-deployment"),
    ] {
        let expected_labels = BTreeMap::from([(
            "svc.dpu.nvidia.com/owned-by-dpudeployment".to_string(),
            format!("{TEST_NS}_{deployment_name}"),
        )]);
        let resource_suffix = format!("-{suffix}");
        let scoped_interfaces = interfaces.iter().filter(|interface| {
            interface
                .metadata
                .name
                .as_deref()
                .is_some_and(|name| name.ends_with(&resource_suffix))
        });
        for interface in scoped_interfaces {
            assert_eq!(
                interface
                    .spec
                    .template
                    .spec
                    .node_selector
                    .as_ref()
                    .and_then(|selector| selector.match_labels.as_ref()),
                Some(&expected_labels)
            );
        }
    }

    // All four configured classes must serialize the same exact topology Patch pairs.
    for suffix in ["bf3", "bf3gb200", "bf4", "astra"] {
        for (logical_name, peer_bridge, peer_patch_name) in [
            ("c2pf3", "br-pf3", "p-pf3"),
            ("c2pf3vf4", "br-vf4", "p-vf4"),
        ] {
            let resource_name = format!("{logical_name}-{suffix}");
            let interface = interfaces
                .iter()
                .find(|interface| {
                    interface.metadata.name.as_deref() == Some(resource_name.as_str())
                })
                .expect("configured scoped Patch interface must exist");
            let logical_label = interface
                .spec
                .template
                .spec
                .template
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.labels.as_ref())
                .and_then(|labels| labels.get("interface"));
            assert_eq!(logical_label.map(String::as_str), Some(logical_name));
            let patch = interface
                .spec
                .template
                .spec
                .template
                .spec
                .patch
                .as_ref()
                .expect("configured interface must be Patch-backed");
            assert_eq!(patch.peer_bridge, peer_bridge);
            assert_eq!(patch.peer_patch_name.as_deref(), Some(peer_patch_name));
        }
    }

    // Astra's CX patch resources must serialize the xplane peer metadata while staying scoped to
    // only the Astra deployment.
    let astra_interface = |logical_name: &str| {
        let resource_name = format!("{logical_name}-astra");
        interfaces
            .iter()
            .find(|interface| interface.metadata.name.as_deref() == Some(resource_name.as_str()))
            .unwrap_or_else(|| panic!("Astra scoped interface {resource_name} must exist"))
    };
    let cx_patch = astra_interface("p-brcx-r0swpln0-to-br-sfc")
        .spec
        .template
        .spec
        .template
        .spec
        .patch
        .as_ref()
        .expect("Astra CX bridge interface must be Patch-backed");
    assert_eq!(cx_patch.peer_bridge, "brcx-r0swpln0");
    assert!(cx_patch.peer_patch_name.is_none());
    assert!(cx_patch.peer_external_i_ds.is_none());
    let xplane_patch = astra_interface("p-br-xplane-r3swpln1-to-br-sfc")
        .spec
        .template
        .spec
        .template
        .spec
        .patch
        .as_ref()
        .expect("Astra xplane interface must be Patch-backed");
    assert_eq!(xplane_patch.peer_bridge, "br-xplane");
    assert!(xplane_patch.peer_patch_name.is_none());
    assert_eq!(
        xplane_patch.peer_external_i_ds.as_ref(),
        Some(&BTreeMap::from([
            ("xplane".to_string(), "true".to_string()),
            ("xplane-group-id".to_string(), "r3swpln1".to_string()),
            ("xplane-downlink".to_string(), "patch".to_string()),
        ]))
    );

    let configured_deployments = [
        // BF3 must render the selected raw PF while consuming the shared topology inventory.
        (
            "bf3-deployment",
            DpuDeploymentType::Bf3,
            "host_representor='pf3hpf'",
            false,
        ),
        // GB200 BF3 shares BF3 networking while carrying the specialized NVConfig profile.
        (
            "bf3-gb200-deployment",
            DpuDeploymentType::Bf3Gb200,
            "host_representor='pf3hpf'",
            true,
        ),
        // Generic BF4 must resolve the selected PF by its exact semantic identity.
        (
            "bf4-deployment",
            DpuDeploymentType::Bf4Generic,
            "resolve_dpf_pf 'c2pf3'",
            false,
        ),
    ];
    // Each tuple identifies the deployment to inspect, the class-label source, and one
    // platform-specific OVS fragment proving that deployment received the correct flavor.
    for (deployment_name, deployment_type, expected_ovs_marker, expects_gb200_profile) in
        configured_deployments
    {
        // The deployment-referenced flavor must contain the configured topology and SF total.
        let deployment = DpuDeploymentRepository::get(&mock, deployment_name, TEST_NS)
            .await
            .unwrap()
            .expect("configured deployment must exist");
        let flavor_name = deployment.spec.dpus.flavor.as_deref().unwrap();
        let flavor = DpuFlavorRepository::get(&mock, flavor_name, TEST_NS)
            .await
            .unwrap()
            .expect("deployment-referenced flavor must exist");
        let nvconfig = flavor.spec.nvconfig.as_ref().unwrap()[0]
            .parameters
            .as_ref()
            .unwrap();
        let expected_pf_total_sf = match deployment_type {
            DpuDeploymentType::Bf3Gb200 => "PF_TOTAL_SF=128",
            DpuDeploymentType::Bf3 | DpuDeploymentType::Bf4Generic => "PF_TOTAL_SF=37",
            DpuDeploymentType::Bf4Astra => unreachable!("Astra is checked separately"),
        };
        assert!(
            nvconfig
                .iter()
                .any(|parameter| parameter == expected_pf_total_sf)
        );
        assert_eq!(
            nvconfig
                .iter()
                .any(|parameter| parameter == "OFF_BOARD_SERIALIZER=1"),
            expects_gb200_profile
        );
        let ovs_script = flavor
            .spec
            .ovs
            .as_ref()
            .and_then(|ovs| ovs.raw_config_script.as_ref())
            .unwrap();
        assert!(ovs_script.contains("add-br 'br-pf3'"));
        assert!(ovs_script.contains("add-br 'br-vf4'"));
        assert!(ovs_script.contains(expected_ovs_marker));

        let expected_labels = InitializationLabeler
            .node_labels_for_deployment_type(deployment_type)
            .unwrap();
        assert_eq!(
            deployment.spec.dpus.dpu_sets.as_ref().unwrap()[0]
                .dpu_node_selector
                .as_ref()
                .and_then(|selector| selector.match_labels.as_ref()),
            Some(&expected_labels)
        );

        // Service-chain ports must expose the exact runtime endpoints for the same inventory.
        let switches = &deployment.spec.service_chains.as_ref().unwrap().switches;
        let chain_interfaces = switches
            .iter()
            .map(|switch| {
                switch.ports[0]
                    .service_interface
                    .as_ref()
                    .unwrap()
                    .match_labels["interface"]
                    .as_str()
            })
            .collect::<Vec<_>>();
        assert_eq!(chain_interfaces, ["p0", "p1", "c2pf3", "c2pf3vf4"]);
        let chain_endpoints = switches
            .iter()
            .flat_map(|switch| {
                let interface_name = switch.ports[0]
                    .service_interface
                    .as_ref()
                    .unwrap()
                    .match_labels["interface"]
                    .as_str();
                switch
                    .ports
                    .iter()
                    .filter_map(|port| port.service.as_ref())
                    .map(move |service| {
                        (
                            interface_name,
                            service.name.as_str(),
                            service.interface.as_str(),
                        )
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            chain_endpoints,
            [
                ("p0", DOCA_HBN_SERVICE_NAME, "p0_if"),
                ("p1", DOCA_HBN_SERVICE_NAME, "p1_if"),
                ("c2pf3", DOCA_HBN_SERVICE_NAME, "pf0hpf_if"),
                ("c2pf3", DHCP_SERVER_SERVICE_NAME, "d_pf0hpf_if"),
                ("c2pf3", FMDS_SERVICE_NAME, "f_pf0hpf_if"),
                ("c2pf3vf4", DOCA_HBN_SERVICE_NAME, "pf0vf4_if"),
                ("c2pf3vf4", DHCP_SERVER_SERVICE_NAME, "d_pf0vf4_if"),
            ]
        );
    }

    // Astra mode sets `astraEnabled` and retains the labeler's Astra-class node selector.
    let astra = DpuDeploymentRepository::get(&mock, "astra-deployment", TEST_NS)
        .await
        .unwrap()
        .expect("Astra deployment must exist");
    assert_eq!(astra.spec.dpus.astra_enabled, Some(true));
    let expected_astra_labels = InitializationLabeler
        .node_labels_for_deployment_type(DpuDeploymentType::Bf4Astra)
        .unwrap();
    assert_eq!(
        astra.spec.dpus.dpu_sets.as_ref().unwrap()[0]
            .dpu_node_selector
            .as_ref()
            .and_then(|selector| selector.match_labels.as_ref()),
        Some(&expected_astra_labels)
    );
    let astra_switches = &astra.spec.service_chains.as_ref().unwrap().switches;
    // Each service chain and fixed xplane pair must appear once; the set checks below hide
    // duplicates.
    assert_eq!(
        astra_switches.len(),
        astra_chainable_logical_names.len() + astra_xplane_group_ids.len()
    );
    // Astra service-to-service chains must select the configured topology logical names while its
    // fixed xplane-to-CX chains remain present.
    let astra_chain_interfaces = astra_switches
        .iter()
        .filter(|switch| switch.ports.iter().any(|port| port.service.is_some()))
        .map(|switch| {
            switch.ports[0]
                .service_interface
                .as_ref()
                .unwrap()
                .match_labels["interface"]
                .clone()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(astra_chain_interfaces, astra_chainable_logical_names);
    let astra_patch_chain_pairs = astra_switches
        .iter()
        .filter_map(|switch| {
            let ports = switch
                .ports
                .iter()
                .filter_map(|port| port.service_interface.as_ref())
                .map(|service_interface| service_interface.match_labels["interface"].clone())
                .collect::<Vec<_>>();
            (ports.len() == 2).then(|| (ports[0].clone(), ports[1].clone()))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        astra_patch_chain_pairs,
        astra_xplane_group_ids
            .into_iter()
            .map(|group_id| {
                (
                    format!("p-brcx-{group_id}-to-br-sfc"),
                    format!("p-br-xplane-{group_id}-to-br-sfc"),
                )
            })
            .collect::<BTreeSet<_>>()
    );
    // One selected PF and one VF produce seven topology-backed endpoints. Astra adds eight Weave
    // DHCP Agent SFs and four headroom SFs, so this fixture must emit exactly PF_TOTAL_SF=19.
    assert!(astra.spec.dpus.flavor.is_none());
    let astra_template = DpuFlavorTemplateRepository::get(
        &mock,
        astra.spec.dpus.flavor_template.as_deref().unwrap(),
        TEST_NS,
    )
    .await
    .unwrap()
    .expect("Astra deployment-referenced flavor template must exist");
    assert!(astra_template.spec.template.starts_with("spec:\n"));
    let astra_flavor = DPUFlavor {
        metadata: Default::default(),
        spec: {
            let body: serde_yaml::Value =
                serde_yaml::from_str(&astra_template.spec.template).unwrap();
            serde_yaml::from_value(body["spec"].clone()).unwrap()
        },
    };
    assert!(
        astra_flavor.spec.nvconfig.as_ref().unwrap()[0]
            .parameters
            .as_ref()
            .unwrap()
            .iter()
            .any(|parameter| parameter == "PF_TOTAL_SF=19")
    );
    assert!(
        astra_flavor.spec.nvconfig.as_ref().unwrap()[0]
            .parameters
            .as_ref()
            .unwrap()
            .iter()
            .any(|parameter| parameter == "DELAY_HOST_OS_INIT=0x3")
    );
    assert!(matches!(
        astra_flavor
            .spec
            .service_readiness
            .and_then(|readiness| readiness.gate),
        Some(DpuFlavorServiceReadinessGate::DpuServiceCriticalPodsReady)
    ));

    // Astra must resolve BF4 identities before fixed OVS setup, then add configured intercept bridges afterward.
    let astra_ovs = astra_flavor
        .spec
        .ovs
        .as_ref()
        .and_then(|ovs| ovs.raw_config_script.as_ref())
        .unwrap();
    assert!(astra_ovs.contains("resolve_dpf_pf 'c2pf3'"));
    assert!(astra_ovs.contains("_ovs-vsctl --may-exist add-br 'br-pf3'"));
    assert!(astra_ovs.contains("_ovs-vsctl --may-exist add-br 'br-vf4'"));
    let resolution = astra_ovs.find("resolve_dpf_pf 'c2pf3'").unwrap();
    let fixed_ovs_setup = astra_ovs.find("/etc/mellanox/ovs-script.sh").unwrap();
    let fixed_xplane_setup = astra_ovs.find("/etc/mellanox/xplane-bridge.sh").unwrap();
    let intercept_bridge_setup = astra_ovs
        .find("_ovs-vsctl --may-exist add-br 'br-pf3'")
        .unwrap();
    assert!(resolution < fixed_ovs_setup);
    assert!(fixed_xplane_setup < intercept_bridge_setup);

    // The flavor ACL must cover the selected topology-backed HBN endpoints and omit an unselected
    // VF.
    let astra_dhcp_acl = astra_flavor
        .spec
        .config_files
        .as_ref()
        .unwrap()
        .iter()
        .find(|file| file.path.ends_with("/10-dhcp.rules"))
        .and_then(|file| file.raw.as_ref())
        .unwrap();
    assert!(astra_dhcp_acl.contains("pf0hpf_if"));
    assert!(astra_dhcp_acl.contains("pf0vf4_if"));
    assert!(!astra_dhcp_acl.contains("pf0vf0_if"));
    drop(sdk);
}

/// Verifies the one-way Astra migration deletes exactly the old scoped PF/VF set and a restarted
/// attempt resumes an accepted deletion, preventing interruption from leaving old and new
/// inventories together.
#[tokio::test(start_paused = true)]
async fn topology_backed_astra_initialization_replaces_pre_topology_scoped_pf_vfs_in_order() {
    // Create the scoped topology-free Astra state emitted by the predecessor startup path.
    let mock = InitializationMock::default();
    let sdk = seed_pre_topology_scoped_astra(&mock).await;

    // Seed one static PF from each unrelated scoped class. Cloning preserves the complete PF
    // fixture, while the non-Astra name keeps it outside the exact migration set. The owner label
    // only keeps the fixture consistent with its deployment class.
    let scoped_pf = DpuServiceInterfaceRepository::get(&mock, "pf0hpf-astra", TEST_NS)
        .await
        .unwrap()
        .expect("the predecessor Astra state must contain its static PF");
    for (name, deployment_name) in [
        // BF3 proves cleanup cannot broaden to another deployment's static PF generation.
        ("pf0hpf-bf3", "bf3-deployment"),
        // Generic BF4 differs by suffix and protects the other supported BF generation.
        ("pf0hpf-bf4", "bf4-deployment"),
    ] {
        let mut interface = scoped_pf.clone();
        interface.metadata.name = Some(name.to_string());
        interface
            .spec
            .template
            .spec
            .node_selector
            .as_mut()
            .expect("the cloned scoped interface must retain its selector")
            .match_labels = Some(BTreeMap::from([(
            "svc.dpu.nvidia.com/owned-by-dpudeployment".to_string(),
            format!("{TEST_NS}_{deployment_name}"),
        )]));
        mock.service_interfaces
            .insert(resource_key(&interface), interface);
    }

    // Pin the predecessor PF/VF generation so migration cleanup cannot silently broaden.
    let pre_topology_names = ["pf0hpf-astra", "pf1hpf-astra"]
        .into_iter()
        .map(str::to_string)
        .chain((0..=13).map(|vf_id| format!("pf0vf{vf_id}-astra")))
        .collect::<BTreeSet<_>>();
    let initial_names = DpuServiceInterfaceRepository::list(&mock, TEST_NS)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|interface| interface.metadata.name)
        .collect::<BTreeSet<_>>();
    assert!(pre_topology_names.is_subset(&initial_names));

    // Hold one accepted delete behind its simulated finalizer so ordering remains observable.
    let deferred_name = "pf0vf13-astra";
    let deferred_key = ns_key(TEST_NS, deferred_name);
    mock.deferred_service_interface_deletes
        .insert(deferred_key.clone(), ());
    let delete_started = mock.service_interface_delete_started.notified();
    let topology_config = InitDpfResourcesConfig {
        deployment_scoped_service_interfaces: true,
        intercept_bridging: Some(configured_intercept_bridging()),
        ..unscoped_astra_config()
    };
    let first_attempt_config = topology_config.clone();
    let mut initialization = tokio::spawn(async move {
        sdk.create_initialization_objects(&first_attempt_config)
            .await
    });

    // The delete must start before task completion so a cleanup regression fails instead of
    // hanging.
    tokio::select! {
        _ = delete_started => {}
        result = &mut initialization => {
            panic!(
                "topology migration completed before submitting its deferred delete: {result:?}"
            );
        }
    }
    tokio::task::yield_now().await;

    // Every historical PF/VF receives one delete request, and retained resources are untouched.
    assert_eq!(
        mock.service_interface_delete_requests.len(),
        pre_topology_names.len()
    );
    for name in &pre_topology_names {
        assert_eq!(
            *mock
                .service_interface_delete_requests
                .get(&ns_key(TEST_NS, name))
                .unwrap_or_else(|| panic!("missing delete request for {name}")),
            1
        );
    }
    for retained_name in [
        "p0-astra",
        "p1-astra",
        "p-brcx-r0swpln0-to-br-sfc-astra",
        "p-br-xplane-r0swpln0-to-br-sfc-astra",
        "pf0hpf-bf3",
        "pf0hpf-bf4",
    ] {
        assert!(
            !mock
                .service_interface_delete_requests
                .contains_key(&ns_key(TEST_NS, retained_name)),
            "{retained_name} must not be deleted by the PF/VF migration"
        );
        assert!(
            DpuServiceInterfaceRepository::get(&mock, retained_name, TEST_NS)
                .await
                .unwrap()
                .is_some()
        );
    }

    // An accepted delete is not completion: replacements remain absent while it still exists.
    assert!(
        DpuServiceInterfaceRepository::get(&mock, deferred_name, TEST_NS)
            .await
            .unwrap()
            .is_some()
    );
    for replacement_name in ["c2pf3-astra", "c2pf3vf4-astra"] {
        assert!(
            DpuServiceInterfaceRepository::get(&mock, replacement_name, TEST_NS)
                .await
                .unwrap()
                .is_none(),
            "{replacement_name} must not coexist with the old scoped PF/VF generation"
        );
    }

    // Crossing the ten-minute diagnostic deadline must not release cleanup or apply replacements.
    tokio::time::advance(Duration::from_secs(10 * 60 + 1)).await;
    assert!(
        !initialization.is_finished(),
        "migration must keep waiting after the blocked-cleanup deadline"
    );
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "c2pf3-astra", TEST_NS)
            .await
            .unwrap()
            .is_none()
    );

    // Interrupt startup while the accepted deletion is still waiting on its finalizer.
    initialization.abort();
    assert!(
        initialization
            .await
            .expect_err("the interrupted initialization task must not complete")
            .is_cancelled()
    );

    // A fresh SDK over the same persisted state must rediscover and resubmit the stale resource.
    let retry_sdk =
        crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
            .with_labeler(InitializationLabeler)
            .build_without_resources()
            .await
            .unwrap();
    let delete_retried = mock.service_interface_delete_started.notified();
    let mut retry = tokio::spawn(async move {
        retry_sdk
            .create_initialization_objects(&topology_config)
            .await
    });
    tokio::select! {
        _ = delete_retried => {}
        result = &mut retry => {
            panic!("retry completed before resubmitting its deferred delete: {result:?}");
        }
    }
    assert_eq!(
        *mock
            .service_interface_delete_requests
            .get(&deferred_key)
            .expect("retry must record the deferred delete"),
        2,
        "restart must resubmit an accepted deletion whose resource still exists"
    );

    // Complete the finalizer only after retry is waiting, then require normal convergence.
    mock.deferred_service_interface_deletes
        .remove(&deferred_key);
    mock.service_interfaces.remove(&deferred_key);
    tokio::time::advance(Duration::from_secs(10)).await;
    retry.await.unwrap().unwrap();

    // The final state contains topology resources and none of the old scoped PF/VF resources.
    let final_names = DpuServiceInterfaceRepository::list(&mock, TEST_NS)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|interface| interface.metadata.name)
        .collect::<BTreeSet<_>>();
    assert!(pre_topology_names.is_disjoint(&final_names));
    assert!(final_names.contains("c2pf3-astra"));
    assert!(final_names.contains("c2pf3vf4-astra"));
    for retained_name in [
        "p0-astra",
        "p1-astra",
        "p-brcx-r0swpln0-to-br-sfc-astra",
        "p-br-xplane-r0swpln0-to-br-sfc-astra",
        "pf0hpf-bf3",
        "pf0hpf-bf4",
    ] {
        assert!(final_names.contains(retained_name));
    }
}

/// Verifies restart completes a partially applied topology before publishing its services and
/// deployment, preventing interrupted startup from exposing references to missing interfaces.
#[tokio::test]
async fn topology_backed_astra_initialization_retries_partial_replacements_before_publication() {
    // Establish the predecessor resources that the migration must replace.
    let mock = InitializationMock::default();
    let sdk = seed_pre_topology_scoped_astra(&mock).await;

    // Retain predecessor defaults and add one marker whose resources identify new publication.
    let marker_service_name = "migration-marker";
    let marker_resource_name = "migration-marker-bf4astra";
    let mut services = crate::services::default_services(&crate::ServiceRegistryConfig::default());
    services.push(ServiceDefinition::new(
        marker_service_name,
        "repo",
        "chart",
        "1.0.0",
    ));
    let topology_config = InitDpfResourcesConfig {
        deployment_scoped_service_interfaces: true,
        intercept_bridging: Some(configured_intercept_bridging()),
        services,
        ..unscoped_astra_config()
    };

    // Hold the second configured replacement before persistence so the first remains observable.
    let deferred_replacement_name = "c2pf3vf4-astra";
    let deferred_replacement_key = ns_key(TEST_NS, deferred_replacement_name);
    mock.deferred_service_interface_applies
        .insert(deferred_replacement_key.clone(), ());
    let replacement_apply_started = mock.service_interface_apply_started.notified();
    let first_attempt_config = topology_config.clone();
    let mut initialization = tokio::spawn(async move {
        sdk.create_initialization_objects(&first_attempt_config)
            .await
    });
    tokio::select! {
        _ = replacement_apply_started => {}
        result = &mut initialization => {
            panic!("initialization completed before reaching its deferred replacement: {result:?}");
        }
    }

    // Cleanup completed and one replacement persisted, but the held replacement remains absent.
    for stale_name in ["pf0hpf-astra", "pf0vf13-astra"] {
        assert!(
            DpuServiceInterfaceRepository::get(&mock, stale_name, TEST_NS)
                .await
                .unwrap()
                .is_none(),
            "{stale_name} must be deleted before replacement application"
        );
    }
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "c2pf3-astra", TEST_NS)
            .await
            .unwrap()
            .is_some(),
        "the first configured replacement must persist before interruption"
    );
    assert!(
        DpuServiceInterfaceRepository::get(&mock, deferred_replacement_name, TEST_NS)
            .await
            .unwrap()
            .is_none(),
        "the deferred replacement must remain absent before interruption"
    );

    // Neither service resources nor the updated deployment may precede the complete interface set.
    assert!(
        DpuServiceTemplateRepository::get(&mock, marker_resource_name, TEST_NS)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        DpuServiceConfigurationRepository::get(&mock, marker_resource_name, TEST_NS)
            .await
            .unwrap()
            .is_none()
    );
    let predecessor_deployment = DpuDeploymentRepository::get(&mock, "astra-deployment", TEST_NS)
        .await
        .unwrap()
        .expect("the predecessor Astra deployment must remain published");
    assert!(
        !predecessor_deployment
            .spec
            .services
            .contains_key(marker_service_name)
    );

    // Cancel the partial attempt, then let a fresh SDK replay the complete desired state.
    initialization.abort();
    assert!(
        initialization
            .await
            .expect_err("the interrupted initialization task must not complete")
            .is_cancelled()
    );
    mock.deferred_service_interface_applies
        .remove(&deferred_replacement_key);
    let retry_sdk =
        crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
            .with_labeler(InitializationLabeler)
            .build_without_resources()
            .await
            .unwrap();
    retry_sdk
        .create_initialization_objects(&topology_config)
        .await
        .expect("retry must complete the partial topology migration");

    // Retry publishes the missing replacement before the marker service and updated deployment.
    assert!(
        DpuServiceInterfaceRepository::get(&mock, deferred_replacement_name, TEST_NS)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        DpuServiceTemplateRepository::get(&mock, marker_resource_name, TEST_NS)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        DpuServiceConfigurationRepository::get(&mock, marker_resource_name, TEST_NS)
            .await
            .unwrap()
            .is_some()
    );
    let deployment = DpuDeploymentRepository::get(&mock, "astra-deployment", TEST_NS)
        .await
        .unwrap()
        .expect("retry must publish the updated Astra deployment");
    assert!(deployment.spec.services.contains_key(marker_service_name));
}

#[tokio::test]
async fn scoped_initialization_prunes_unscoped_interfaces() {
    let definitions = crate::sdk::build_dpu_interfaces_vec();

    // Legacy-to-scoped: legacy interfaces are pruned, including stale interfaces from a
    // previously configured intercept inventory.
    let mock = InitializationMock::default();
    let stale_p1 = crate::sdk::build_service_interface(
        definitions
            .iter()
            .find(|definition| definition.name == "p1")
            .expect("static test inventory must contain p1"),
        TEST_NS,
    );
    mock.service_interfaces
        .insert(resource_key(&stale_p1), stale_p1);
    let old_intercept_interfaces = crate::sdk::build_effective_dpu_interfaces(
        DEFAULT_DPU_NUM_OF_VFS,
        Some(&configured_intercept_bridging()),
    );
    let stale_c2pf3 = crate::sdk::build_service_interface(
        old_intercept_interfaces
            .iter()
            .find(|definition| definition.name == "c2pf3")
            .expect("configured test inventory must contain c2pf3"),
        TEST_NS,
    );
    mock.service_interfaces
        .insert(resource_key(&stale_c2pf3), stale_c2pf3);
    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .with_labeler(InitializationLabeler)
        .initialize(&InitDpfResourcesConfig {
            bfb_url: "http://example.com/test.bfb".to_string(),
            deployment_scoped_service_interfaces: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "p1", TEST_NS)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "p1-bf3", TEST_NS)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "c2pf3", TEST_NS)
            .await
            .unwrap()
            .is_none()
    );
    drop(sdk);
}

/// Scoped cleanup is independent of BF4; shared unscoped PF1 is removed only without BF4.
#[tokio::test]
async fn stale_pf1_cleanup_checks_bf4_only_for_unscoped_interfaces() {
    for (deployment_type, scoped, bf4_configured, deleted_name) in [
        (DpuDeploymentType::Bf3, true, false, Some("pf1hpf-bf3")),
        (DpuDeploymentType::Bf3, true, true, Some("pf1hpf-bf3")),
        (
            DpuDeploymentType::Bf3Gb200,
            true,
            false,
            Some("pf1hpf-bf3gb200"),
        ),
        (
            DpuDeploymentType::Bf3Gb200,
            true,
            true,
            Some("pf1hpf-bf3gb200"),
        ),
        (DpuDeploymentType::Bf3, false, false, Some("pf1hpf")),
        (DpuDeploymentType::Bf3, false, true, None),
    ] {
        let mock = InitializationMock::default();
        let definition = crate::sdk::build_dpu_interfaces_vec()
            .into_iter()
            .find(|interface| interface.name == "pf1hpf")
            .unwrap();
        for name in ["pf1hpf", "pf1hpf-bf3", "pf1hpf-bf3gb200", "pf1hpf-bf4"] {
            let mut interface = crate::sdk::build_service_interface(&definition, TEST_NS);
            interface.metadata.name = Some(name.to_string());
            mock.service_interfaces
                .insert(resource_key(&interface), interface);
        }
        let sdk =
            crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
                .build_without_resources()
                .await
                .unwrap();
        let config = InitDpfResourcesConfig {
            deployment_type,
            deployment_scoped_service_interfaces: scoped,
            ..Default::default()
        };
        sdk.cleanup_stale_pf1_interfaces(&[&config], bf4_configured)
            .await
            .unwrap();
        for name in ["pf1hpf", "pf1hpf-bf3", "pf1hpf-bf3gb200", "pf1hpf-bf4"] {
            assert_eq!(
                mock.service_interfaces.contains_key(&ns_key(TEST_NS, name)),
                Some(name) != deleted_name,
                "deployment={deployment_type:?}, scoped={scoped}, bf4_configured={bf4_configured}, interface={name}"
            );
        }
    }
}

/// BF4 profiles and explicitly configured PF1 prevent cleanup.
#[tokio::test]
async fn stale_pf1_cleanup_preserves_requested_interfaces() {
    let pf1 = crate::sdk::build_dpu_interfaces_vec()
        .into_iter()
        .find(|interface| interface.name == "pf1hpf")
        .unwrap();
    let topology = DpfInterceptBridging::new(
        vec![DpfInterceptBridge {
            identity: DpfInterfaceIdentity {
                controller_id: 0,
                pf_id: 1,
                vf_id: None,
            },
            bridge: "br-host".to_string(),
            patch_port: "host-patch".to_string(),
        }],
        crate::DEFAULT_DPU_NUM_OF_VFS,
    )
    .unwrap();
    for config in [
        InitDpfResourcesConfig {
            deployment_type: DpuDeploymentType::Bf4Generic,
            ..Default::default()
        },
        InitDpfResourcesConfig {
            intercept_bridging: Some(topology),
            ..Default::default()
        },
        InitDpfResourcesConfig {
            interfaces: vec![pf1.clone()],
            ..Default::default()
        },
    ] {
        let mock = InitializationMock::default();
        let interface = crate::sdk::build_service_interface(&pf1, TEST_NS);
        mock.service_interfaces
            .insert(resource_key(&interface), interface);
        let sdk =
            crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
                .build_without_resources()
                .await
                .unwrap();
        sdk.cleanup_stale_pf1_interfaces(&[&config], false)
            .await
            .unwrap();
        assert!(
            mock.service_interfaces
                .contains_key(&ns_key(TEST_NS, "pf1hpf"))
        );
    }
}

/// A successful delete request is insufficient: wait until DPF removes the template.
#[tokio::test(start_paused = true)]
async fn stale_pf1_cleanup_waits_for_actual_deletion() {
    let mock = InitializationMock::default();
    let definition = crate::sdk::build_dpu_interfaces_vec()
        .into_iter()
        .find(|interface| interface.name == "pf1hpf")
        .unwrap();
    let interface = crate::sdk::build_service_interface(&definition, TEST_NS);
    mock.service_interfaces
        .insert(resource_key(&interface), interface);
    mock.deferred_service_interface_deletes
        .insert(ns_key(TEST_NS, "pf1hpf"), ());
    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .build_without_resources()
        .await
        .unwrap();
    let cleanup = tokio::spawn(async move {
        sdk.cleanup_stale_pf1_interfaces(&[&InitDpfResourcesConfig::default()], false)
            .await
    });
    mock.service_interface_delete_started.notified().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        !cleanup.is_finished(),
        "cleanup must wait while the interface still exists"
    );
    mock.service_interfaces.remove(&ns_key(TEST_NS, "pf1hpf"));
    cleanup.await.unwrap().unwrap();
}

/// The deadline covers both a blocked delete request and accepted deletion with stuck finalizers.
#[tokio::test(start_paused = true)]
async fn stale_pf1_cleanup_times_out() {
    for block_delete_request in [true, false] {
        let mock = InitializationMock::default();
        let definition = crate::sdk::build_dpu_interfaces_vec()
            .into_iter()
            .find(|interface| interface.name == "pf1hpf")
            .unwrap();
        let interface = crate::sdk::build_service_interface(&definition, TEST_NS);
        mock.service_interfaces
            .insert(resource_key(&interface), interface);
        let deletes = if block_delete_request {
            &mock.blocked_service_interface_deletes
        } else {
            &mock.deferred_service_interface_deletes
        };
        deletes.insert(ns_key(TEST_NS, "pf1hpf"), ());
        let sdk =
            crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
                .build_without_resources()
                .await
                .unwrap();
        let cleanup = tokio::spawn(async move {
            sdk.cleanup_stale_pf1_interfaces(&[&InitDpfResourcesConfig::default()], false)
                .await
        });
        mock.service_interface_delete_started.notified().await;
        tokio::time::advance(Duration::from_secs(119)).await;
        assert!(
            !cleanup.is_finished(),
            "cleanup must allow the two-minute window"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = cleanup.await.unwrap().unwrap_err();
        assert!(matches!(error, DpfError::Timeout { ref details, .. }
            if details.contains("sdk-init-ns") && details.contains("pf1hpf") && details.contains("two minutes")));
        assert!(
            mock.service_interfaces
                .contains_key(&ns_key(TEST_NS, "pf1hpf")),
            "timing out must leave the interface for DPF to finish deleting"
        );
    }
}

/// Send both scoped deletes before waiting, or a single deduplicated unscoped delete.
/// The whole batch shares one two-minute deadline, even when delete requests are blocked.
#[tokio::test(start_paused = true)]
async fn stale_pf1_cleanup_batches_deletes_with_one_deadline() {
    for scoped in [true, false] {
        let mock = InitializationMock::default();
        let definition = crate::sdk::build_dpu_interfaces_vec()
            .into_iter()
            .find(|interface| interface.name == "pf1hpf")
            .unwrap();
        let names = if scoped {
            vec!["pf1hpf-bf3", "pf1hpf-bf3gb200"]
        } else {
            vec!["pf1hpf"]
        };
        for name in &names {
            let mut interface = crate::sdk::build_service_interface(&definition, TEST_NS);
            interface.metadata.name = Some((*name).to_string());
            mock.service_interfaces
                .insert(resource_key(&interface), interface);
            mock.blocked_service_interface_deletes
                .insert(ns_key(TEST_NS, name), ());
        }
        let sdk =
            crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
                .build_without_resources()
                .await
                .unwrap();
        let cleanup = tokio::spawn(async move {
            let bf3 = InitDpfResourcesConfig {
                deployment_scoped_service_interfaces: scoped,
                ..Default::default()
            };
            let gb200 = InitDpfResourcesConfig {
                deployment_type: DpuDeploymentType::Bf3Gb200,
                deployment_scoped_service_interfaces: scoped,
                ..Default::default()
            };
            sdk.cleanup_stale_pf1_interfaces(&[&bf3, &gb200], false)
                .await
        });
        mock.service_interface_delete_started.notified().await;
        assert_eq!(mock.service_interface_delete_requests.len(), names.len());
        for name in &names {
            assert_eq!(
                *mock
                    .service_interface_delete_requests
                    .get(&ns_key(TEST_NS, name))
                    .unwrap(),
                1
            );
        }
        tokio::time::advance(Duration::from_secs(119)).await;
        assert!(!cleanup.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = cleanup.await.unwrap().unwrap_err();
        assert!(matches!(error, DpfError::Timeout { ref details, .. }
            if names.iter().all(|name| details.contains(name))));
    }
}

#[tokio::test]
async fn scoped_initialization_waits_for_legacy_interface_deletion() {
    let definitions = crate::sdk::build_dpu_interfaces_vec();
    let mock = InitializationMock::default();
    let stale_p1 = crate::sdk::build_service_interface(
        definitions
            .iter()
            .find(|definition| definition.name == "p1")
            .expect("static test inventory must contain p1"),
        TEST_NS,
    );
    mock.service_interfaces
        .insert(resource_key(&stale_p1), stale_p1);
    mock.blocked_service_interface_deletes
        .insert(ns_key(TEST_NS, "p1"), ());

    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .with_labeler(InitializationLabeler)
        .build_without_resources()
        .await
        .unwrap();
    let delete_started = mock.service_interface_delete_started.notified();
    let initialization = tokio::spawn(async move {
        sdk.create_initialization_objects(&InitDpfResourcesConfig {
            bfb_url: "http://example.com/test.bfb".to_string(),
            deployment_scoped_service_interfaces: true,
            ..Default::default()
        })
        .await
    });

    delete_started.await;
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "p1-bf3", TEST_NS)
            .await
            .unwrap()
            .is_none(),
        "scoped replacement must not be created before legacy deletion completes"
    );

    mock.blocked_service_interface_deletes
        .remove(&ns_key(TEST_NS, "p1"));
    mock.service_interface_delete_release.notify_one();
    initialization.await.unwrap().unwrap();
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "p1-bf3", TEST_NS)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn scoped_initialization_keeps_waiting_when_legacy_interface_delete_hangs() {
    let definitions = crate::sdk::build_dpu_interfaces_vec();
    let mock = InitializationMock::default();
    let stale_p1 = crate::sdk::build_service_interface(
        definitions
            .iter()
            .find(|definition| definition.name == "p1")
            .expect("static test inventory must contain p1"),
        TEST_NS,
    );
    mock.service_interfaces
        .insert(resource_key(&stale_p1), stale_p1);
    mock.blocked_service_interface_deletes
        .insert(ns_key(TEST_NS, "p1"), ());

    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .with_labeler(InitializationLabeler)
        .build_without_resources()
        .await
        .unwrap();
    let delete_started = mock.service_interface_delete_started.notified();
    let initialization = tokio::spawn(async move {
        sdk.create_initialization_objects(&InitDpfResourcesConfig {
            bfb_url: "http://example.com/test.bfb".to_string(),
            deployment_scoped_service_interfaces: true,
            ..Default::default()
        })
        .await
    });

    delete_started.await;
    tokio::time::advance(Duration::from_secs(10 * 60 + 1)).await;
    assert!(
        !initialization.is_finished(),
        "a blocked migration must continue waiting after the operator-error log"
    );
    mock.blocked_service_interface_deletes
        .remove(&ns_key(TEST_NS, "p1"));
    mock.service_interface_delete_release.notify_one();
    initialization.await.unwrap().unwrap();
    assert!(
        DpuServiceInterfaceRepository::get(&mock, "p1-bf3", TEST_NS)
            .await
            .unwrap()
            .is_some(),
        "scoped replacements must be created after the blocked cleanup completes"
    );
}

/// Verifies a missing referenced template fails the complete inventory lookup
/// so callers cannot mistake an incomplete operator view for current state.
#[tokio::test]
async fn service_versions_fail_when_referenced_template_is_missing() {
    let mock = InitializationMock::default();
    let dpu_name = "node-host-device-dpu";
    let mut dpu = super::helpers::make_dpu(
        TEST_NS,
        dpu_name,
        "device-dpu",
        "node-host",
        DpuStatusPhase::Ready,
    );
    dpu.metadata.labels = Some(BTreeMap::from([(
        "svc.dpu.nvidia.com/owned-by-dpudeployment".to_string(),
        format!("{TEST_NS}_deployment"),
    )]));
    mock.dpus.insert(resource_key(&dpu), dpu);

    // Resolve one service before encountering the absent template to exercise
    // the partial-result path that must now be rejected.
    let services = vec![
        ServiceDefinition::new("a-present", "repo", "chart", "1.0.0"),
        ServiceDefinition::new("z-missing", "repo", "chart", "2.0.0"),
    ];
    let deployment = crate::sdk::build_deployment(
        &services,
        "deployment",
        &crate::sdk::DpuProvisioningSource::Bfb("bfb".to_string()),
        "flavor",
        TEST_NS,
        &[],
        BTreeMap::new(),
        crate::types::DpuDeploymentType::Bf3,
    );
    DpuDeploymentRepository::apply(&mock, &deployment)
        .await
        .unwrap();
    DpuServiceTemplateRepository::apply(
        &mock,
        &crate::sdk::build_service_template(&services[0], TEST_NS, ""),
    )
    .await
    .unwrap();
    let sdk = crate::sdk::DpfSdkBuilder::new(mock, TEST_NS, String::new())
        .build_without_resources()
        .await
        .unwrap();

    // The missing reference invalidates the whole snapshot rather than
    // returning only the service whose template was available.
    let error = sdk
        .get_service_versions_for_dpu(dpu_name)
        .await
        .expect_err("missing referenced template must fail inventory lookup");
    let DpfError::InvalidState(message) = error else {
        panic!("expected invalid state, got {error}");
    };
    assert!(message.contains(
        "DPUServiceTemplate z-missing not found for service z-missing in DPUDeployment deployment"
    ));
}

/// Re-initialization must not overwrite an operator's extra-script content.
///
/// carbide-api seeds these ConfigMaps on every startup, so the create-only path
/// is the only thing standing between a restart and a clobbered site script.
#[tokio::test]
async fn reinitialization_preserves_operator_extra_scripts() {
    let mock = InitializationMock::default();
    let sdk = crate::sdk::DpfSdkBuilder::new(mock.clone(), TEST_NS, "test-password".to_string())
        .with_labeler(InitializationLabeler)
        .build_without_resources()
        .await
        .unwrap();

    let services = [
        DTS_SERVICE_NAME,
        DOCA_HBN_SERVICE_NAME,
        DPU_AGENT_SERVICE_NAME,
        DHCP_SERVER_SERVICE_NAME,
        FMDS_SERVICE_NAME,
        OTEL_COLLECTOR_SERVICE_NAME,
    ]
    .into_iter()
    .map(|name| ServiceDefinition::new(name, "repo", "chart", "1.0.0"))
    .collect::<Vec<_>>();
    let config = InitDpfResourcesConfigBuilder::default()
        .bluefield_software(BlueFieldSoftwareParams {
            os_iso: "http://example.com/bf4.iso".to_string(),
            pldm_fw_bundle: Some(BTreeMap::from([(
                "pldmid001".to_string(),
                "http://example.com/bf4.pldm".to_string(),
            )])),
        })
        .deployment_name("bf4-deployment")
        .flavor_name("bf4-flavor")
        .services(services)
        .deployment_type(DpuDeploymentType::Bf4Generic)
        .build()
        .expect("extra-script test configuration must be valid");

    sdk.create_initialization_objects(&config).await.unwrap();

    let key = ns_key(TEST_NS, "extra-script-pre-ovs-bf4-generic");
    assert!(
        mock.configs.contains_key(&key),
        "first initialization seeds the hook ConfigMap"
    );

    // Stand in for an operator replacing the placeholder with a site script.
    let operator_script = "#!/usr/bin/env bash\necho site-specific\n";
    mock.configs.insert(
        key.clone(),
        BTreeMap::from([("script".to_string(), operator_script.to_string())]),
    );

    sdk.create_initialization_objects(&config).await.unwrap();

    assert_eq!(
        mock.configs.get(&key).unwrap().get("script").unwrap(),
        operator_script,
        "a restart must leave the operator's script alone"
    );
}
