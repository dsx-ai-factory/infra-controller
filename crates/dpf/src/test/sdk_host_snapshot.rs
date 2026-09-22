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

//! `DpfSdk::snapshot_host`, the ad-hoc dump behind `nico-admin-cli dpf
//! snapshot`. The status fields it carries are the point of the command, so
//! these cover what survives the walk from DPU CR to `DpuSummary` — including
//! a DPU with no status at all, which is what a freshly created CR looks like.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use kube::core::ObjectMeta;

use crate::crds::dpudevices_generated::DPUDevice;
use crate::crds::dpunodes_generated::DPUNode;
use crate::crds::dpus_generated::DPU;
use crate::error::DpfError;
use crate::repository::{
    DpuDeviceRepository, DpuNodeRepository, DpuRepository, K8sConfigRepository,
};
use crate::sdk::DpfSdkBuilder;
use crate::types::HostDpfSnapshot;

const TEST_NS: &str = "test-namespace";
const NODE_ID: &str = "aabbccddeeff";
const DEVICE_ID: &str = "0011";

fn node_cr_name() -> String {
    format!("node-{NODE_ID}")
}

fn device_cr_name() -> String {
    format!("device-{DEVICE_ID}")
}

fn dpu_cr_name() -> String {
    format!("node-{NODE_ID}-device-{DEVICE_ID}")
}

#[derive(Default)]
struct SnapshotMock {
    nodes: Arc<DashMap<String, DPUNode>>,
    devices: Arc<DashMap<String, DPUDevice>>,
    dpus: Arc<DashMap<String, DPU>>,
}

impl SnapshotMock {
    /// A node pointing at one device, that device, and `dpu` under the CR name
    /// `snapshot_host` derives from the two.
    fn with(dpu: DPU) -> Self {
        let mock = Self::default();
        mock.nodes.insert(node_cr_name(), node());
        mock.devices.insert(device_cr_name(), device());
        mock.dpus.insert(dpu_cr_name(), dpu);
        mock
    }
}

#[async_trait]
impl DpuNodeRepository for SnapshotMock {
    async fn get(&self, name: &str, _ns: &str) -> Result<Option<DPUNode>, DpfError> {
        Ok(self.nodes.get(name).map(|n| n.clone()))
    }
    async fn list(&self, _ns: &str) -> Result<Vec<DPUNode>, DpfError> {
        Ok(self.nodes.iter().map(|n| n.clone()).collect())
    }
    async fn create(&self, node: &DPUNode) -> Result<DPUNode, DpfError> {
        Ok(node.clone())
    }
    async fn patch(&self, _: &str, _: &str, _: serde_json::Value) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete(&self, _: &str, _: &str) -> Result<(), DpfError> {
        Ok(())
    }
}

#[async_trait]
impl DpuDeviceRepository for SnapshotMock {
    async fn get(&self, name: &str, _ns: &str) -> Result<Option<DPUDevice>, DpfError> {
        Ok(self.devices.get(name).map(|d| d.clone()))
    }
    async fn list(&self, _ns: &str) -> Result<Vec<DPUDevice>, DpfError> {
        Ok(self.devices.iter().map(|d| d.clone()).collect())
    }
    async fn create(&self, device: &DPUDevice) -> Result<DPUDevice, DpfError> {
        Ok(device.clone())
    }
    async fn patch(&self, _: &str, _: &str, _: serde_json::Value) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete(&self, _: &str, _: &str) -> Result<(), DpfError> {
        Ok(())
    }
}

#[async_trait]
impl DpuRepository for SnapshotMock {
    async fn get(&self, name: &str, _ns: &str) -> Result<Option<DPU>, DpfError> {
        Ok(self.dpus.get(name).map(|d| d.clone()))
    }
    async fn list(&self, _ns: &str, _selector: Option<&str>) -> Result<Vec<DPU>, DpfError> {
        Ok(self.dpus.iter().map(|d| d.clone()).collect())
    }
    async fn patch_status(&self, _: &str, _: &str, _: serde_json::Value) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete(&self, _: &str, _: &str) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete_if_uid(&self, name: &str, _ns: &str, uid: &str) -> Result<(), DpfError> {
        let current_uid = self
            .dpus
            .get(name)
            .map(|dpu| dpu.metadata.uid.clone())
            .ok_or_else(|| DpfError::not_found("DPU", name))?;
        if current_uid.as_deref() != Some(uid) {
            return Err(DpfError::InvalidState(format!(
                "DPU {name} no longer has UID {uid}"
            )));
        }
        self.dpus.remove(name);
        Ok(())
    }
    fn watch<F, Fut>(
        &self,
        _namespace: &str,
        _label_selector: Option<&str>,
        _handler: F,
    ) -> impl std::future::Future<Output = ()> + Send + 'static
    where
        F: Fn(Arc<DPU>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        std::future::ready(())
    }
}

#[async_trait]
impl K8sConfigRepository for SnapshotMock {
    async fn get_configmap(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DpfError> {
        Ok(None)
    }
    async fn create_configmap(
        &self,
        _: &str,
        _: &str,
        _: BTreeMap<String, String>,
    ) -> Result<bool, DpfError> {
        Ok(true)
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

fn node() -> DPUNode {
    DPUNode {
        metadata: ObjectMeta {
            name: Some(node_cr_name()),
            namespace: Some(TEST_NS.to_string()),
            ..Default::default()
        },
        spec: serde_json::from_value(serde_json::json!({
            "dpus": [{ "name": device_cr_name() }],
        }))
        .expect("valid DpuNodeSpec"),
        status: None,
    }
}

fn device() -> DPUDevice {
    DPUDevice {
        metadata: ObjectMeta {
            name: Some(device_cr_name()),
            namespace: Some(TEST_NS.to_string()),
            ..Default::default()
        },
        spec: serde_json::from_value(serde_json::json!({
            "bmcIp": "192.0.2.10",
            "bmcPort": 443,
            "serialNumber": "MT2000X00000",
        }))
        .expect("valid DpuDeviceSpec"),
        status: None,
    }
}

/// `status` is passed through verbatim so a test can hand it exactly the shape
/// the CRD produces, rather than assembling generated structs field by field.
fn dpu(status: Option<serde_json::Value>) -> DPU {
    DPU {
        metadata: ObjectMeta {
            name: Some(dpu_cr_name()),
            namespace: Some(TEST_NS.to_string()),
            ..Default::default()
        },
        spec: serde_json::from_value(serde_json::json!({
            "bfb": "bf-bundle-abc",
            "dpuFlavor": "test-flavor",
            "dpuDeviceName": device_cr_name(),
            "dpuNodeName": node_cr_name(),
            "nodeEffect": { "noEffect": true },
            "serialNumber": "MT2000X00000",
        }))
        .expect("valid DpuSpec"),
        status: status.map(|s| serde_json::from_value(s).expect("valid DpuStatus")),
    }
}

async fn snapshot(mock: SnapshotMock) -> HostDpfSnapshot {
    DpfSdkBuilder::new(mock, TEST_NS, String::new())
        .build_without_resources()
        .await
        .expect("sdk")
        .snapshot_host(&node_cr_name())
        .await
        .expect("snapshot")
}

/// A ready DPU reporting all three status surfaces at once. `phase` alone says
/// where a DPU is, not why, so the snapshot is only useful if these survive.
#[tokio::test]
async fn a_dpus_conditions_operational_conditions_and_agent_status_are_all_reported() {
    let mock = SnapshotMock::with(dpu(Some(serde_json::json!({
        "phase": "Ready",
        "bfbFile": "/bfb/test-namespace-bf-bundle-abc.bfb",
        "conditions": [{
            "type": "Ready",
            "status": "True",
            "reason": "DPUReady",
            "message": "DPU is ready",
            "lastTransitionTime": "2026-08-25T00:00:00Z",
            "observedGeneration": 3,
        }],
        "operationalConditions": [{
            "type": "NVConfigUpToDate",
            "status": "False",
            "reason": "PendingReboot",
            "message": "nvconfig applied, awaiting reboot",
            "lastTransitionTime": "2026-08-25T01:00:00Z",
        }],
        "agentStatus": {
            "kubeletVersion": "v1.31.4",
            "initialBootId": "boot-abc",
            "rebootSequenceCount": 2,
            "conditions": [{
                "type": "AgentReady",
                "status": "True",
                "reason": "Running",
                "message": "agent is running",
                "lastTransitionTime": "2026-08-25T02:00:00Z",
            }],
        },
    }))));

    let snapshot = snapshot(mock).await;

    let [dpu] = &snapshot.dpus[..] else {
        panic!("expected exactly one DPU, got {}", snapshot.dpus.len());
    };
    assert_eq!(dpu.status_phase.as_deref(), Some("Ready"));

    let conditions = dpu.status_conditions.as_ref().expect("conditions reported");
    assert_eq!(conditions.len(), 1);
    assert_eq!(conditions[0].type_, "Ready");
    assert_eq!(conditions[0].status, "True");
    assert_eq!(conditions[0].reason, "DPUReady");
    assert_eq!(conditions[0].observed_generation, Some(3));

    let operational = dpu
        .status_operational_conditions
        .as_ref()
        .expect("operational conditions reported");
    assert_eq!(operational.len(), 1);
    assert_eq!(operational[0].r#type, "NVConfigUpToDate");
    assert_eq!(operational[0].reason, "PendingReboot");

    let agent = dpu
        .status_agent_status
        .as_ref()
        .expect("agent status reported");
    assert_eq!(agent.kubelet_version.as_deref(), Some("v1.31.4"));
    assert_eq!(agent.reboot_sequence_count, Some(2));
    // The agent keeps its own conditions, distinct from the DPU's above.
    assert_eq!(
        agent
            .conditions
            .as_ref()
            .map(|c| c.iter().map(|c| c.type_.as_str()).collect::<Vec<_>>()),
        Some(vec!["AgentReady"])
    );
}

/// The three fields are independently optional on the CRD, so a DPU reporting
/// only some of them must not drag the others into existence.
#[tokio::test]
async fn absent_status_fields_are_reported_as_absent_rather_than_empty() {
    let mock = SnapshotMock::with(dpu(Some(serde_json::json!({
        "phase": "Initializing",
        "conditions": [{
            "type": "Initialized",
            "status": "False",
            "reason": "Pending",
            "message": "waiting on BFB",
            "lastTransitionTime": "2026-08-25T00:00:00Z",
        }],
    }))));

    let snapshot = snapshot(mock).await;

    let [dpu] = &snapshot.dpus[..] else {
        panic!("expected exactly one DPU, got {}", snapshot.dpus.len());
    };
    assert!(dpu.status_conditions.is_some());
    assert!(dpu.status_operational_conditions.is_none());
    assert!(dpu.status_agent_status.is_none());
}

/// A DPU CR exists before the operator writes any status to it. Reporting the
/// DPU with empty status beats omitting it, which would read as "no such DPU".
#[tokio::test]
async fn a_dpu_without_status_is_still_reported() {
    let mock = SnapshotMock::with(dpu(None));

    let snapshot = snapshot(mock).await;

    let [dpu] = &snapshot.dpus[..] else {
        panic!("expected exactly one DPU, got {}", snapshot.dpus.len());
    };
    assert_eq!(dpu.name, dpu_cr_name());
    assert!(dpu.status_phase.is_none());
    assert!(dpu.status_conditions.is_none());
    assert!(dpu.status_operational_conditions.is_none());
    assert!(dpu.status_agent_status.is_none());
}
