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

//! Tests for DPU device and node registration and force delete.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kube::Resource;

use crate::crds::dpudevices_generated::DPUDevice;
use crate::crds::dpunodes_generated::*;
use crate::crds::dpus_generated::*;
use crate::error::DpfError;
use crate::repository::{
    DpfOperatorConfigRepository, DpuDeviceRepository, DpuNodeRepository, DpuRepository,
    K8sConfigRepository,
};
use crate::sdk::DpfSdkBuilder;
use crate::types::*;

const TEST_NS: &str = "sdk-device-reg-ns";

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

#[derive(Clone, Default)]
struct DeviceRegistrationMock {
    devices: Arc<RwLock<BTreeMap<String, DPUDevice>>>,
    nodes: Arc<RwLock<BTreeMap<String, DPUNode>>>,
}

#[async_trait]
impl DpuDeviceRepository for DeviceRegistrationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUDevice>, DpfError> {
        Ok(self.devices.read().unwrap().get(&ns_key(ns, name)).cloned())
    }
    async fn list(&self, ns: &str) -> Result<Vec<DPUDevice>, DpfError> {
        Ok(self
            .devices
            .read()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(&format!("{}/", ns)))
            .map(|(_, v)| v.clone())
            .collect())
    }
    async fn create(&self, d: &DPUDevice) -> Result<DPUDevice, DpfError> {
        self.devices
            .write()
            .unwrap()
            .insert(resource_key(d), d.clone());
        Ok(d.clone())
    }
    async fn patch(&self, name: &str, ns: &str, patch: serde_json::Value) -> Result<(), DpfError> {
        let mut devices = self.devices.write().unwrap();
        let device = devices
            .get_mut(&ns_key(ns, name))
            .ok_or_else(|| DpfError::not_found("DPUDevice", name))?;

        let Some(node_labels) = patch
            .pointer("/spec/cluster/nodeLabels")
            .and_then(serde_json::Value::as_object)
        else {
            return Ok(());
        };

        let cluster = device.spec.cluster.get_or_insert({
            crate::crds::dpudevices_generated::DpuDeviceCluster {
                node_annotations: None,
                node_labels: None,
            }
        });
        let labels = cluster.node_labels.get_or_insert_with(BTreeMap::new);
        for (key, value) in node_labels {
            if value.is_null() {
                labels.remove(key);
            } else if let Some(value) = value.as_str() {
                labels.insert(key.clone(), value.to_owned());
            }
        }
        Ok(())
    }
    async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
        self.devices.write().unwrap().remove(&ns_key(ns, name));
        Ok(())
    }
}

#[async_trait]
impl DpuNodeRepository for DeviceRegistrationMock {
    async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUNode>, DpfError> {
        Ok(self.nodes.read().unwrap().get(&ns_key(ns, name)).cloned())
    }
    async fn list(&self, ns: &str) -> Result<Vec<DPUNode>, DpfError> {
        Ok(self
            .nodes
            .read()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(&format!("{}/", ns)))
            .map(|(_, v)| v.clone())
            .collect())
    }
    async fn create(&self, n: &DPUNode) -> Result<DPUNode, DpfError> {
        self.nodes
            .write()
            .unwrap()
            .insert(resource_key(n), n.clone());
        Ok(n.clone())
    }
    async fn patch(&self, _: &str, _: &str, _: serde_json::Value) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
        self.nodes.write().unwrap().remove(&ns_key(ns, name));
        Ok(())
    }
}

#[async_trait]
impl DpuRepository for DeviceRegistrationMock {
    async fn get(&self, _: &str, _: &str) -> Result<Option<DPU>, DpfError> {
        Ok(None)
    }
    async fn list(&self, _: &str, _: Option<&str>) -> Result<Vec<DPU>, DpfError> {
        Ok(vec![])
    }
    async fn patch_status(&self, _: &str, _: &str, _: serde_json::Value) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete(&self, _: &str, _: &str) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete_if_uid(&self, name: &str, _ns: &str, _uid: &str) -> Result<(), DpfError> {
        Err(DpfError::not_found("DPU", name))
    }
    fn watch<F, Fut>(
        &self,
        _: &str,
        _: Option<&str>,
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
impl K8sConfigRepository for DeviceRegistrationMock {
    async fn create_configmap(
        &self,
        _name: &str,
        _ns: &str,
        _data: BTreeMap<String, String>,
    ) -> Result<bool, DpfError> {
        Ok(true)
    }

    async fn get_configmap(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DpfError> {
        Ok(None)
    }
    async fn apply_configmap(
        &self,
        _: &str,
        _: &str,
        _: BTreeMap<String, String>,
    ) -> Result<(), DpfError> {
        Ok(())
    }
    async fn get_secret(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, DpfError> {
        Ok(None)
    }
    async fn apply_secret(
        &self,
        _: &str,
        _: &str,
        _: BTreeMap<String, Vec<u8>>,
    ) -> Result<(), DpfError> {
        Ok(())
    }
}

#[async_trait]
impl DpfOperatorConfigRepository for DeviceRegistrationMock {
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

#[tokio::test]
async fn test_register_devices_node_and_force_delete() {
    let mock = DeviceRegistrationMock::default();
    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .build_without_resources()
        .await
        .unwrap();

    // Register devices
    for i in 1..=2 {
        let info = DpuDeviceInfo {
            device_id: format!("dpu-{}", i),
            dpu_bmc_ip: format!("192.168.1.{}", 100 + i).parse().unwrap(),
            host_bmc_ip: "192.168.1.1".parse().unwrap(),
            serial_number: format!("SN-{}", i),
            dpu_machine_id: format!("dpu-{}-id", i),
            is_primary: true,
        };
        sdk.register_dpu_device(info, None).await.unwrap();
    }

    // Register node
    let node_info = DpuNodeInfo {
        node_id: "host-001".to_string(),
        host_bmc_ip: "192.168.1.1".parse().unwrap(),
        device_ids: vec!["dpu-1".to_string(), "dpu-2".to_string()],
        deployment_type: DpuDeploymentType::Bf3,
    };
    sdk.register_dpu_node(node_info).await.unwrap();

    assert_eq!(
        DpuDeviceRepository::list(&mock, TEST_NS)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        DpuNodeRepository::list(&mock, TEST_NS).await.unwrap().len(),
        1
    );

    // Force delete
    let dpu_ids = vec!["dpu-1".to_string(), "dpu-2".to_string()];
    sdk.force_delete_host("host-001", &dpu_ids).await.unwrap();

    assert_eq!(
        DpuDeviceRepository::list(&mock, TEST_NS)
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        DpuNodeRepository::list(&mock, TEST_NS).await.unwrap().len(),
        0
    );
}
