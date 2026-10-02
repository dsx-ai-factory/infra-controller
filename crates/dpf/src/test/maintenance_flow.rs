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

//! Tests for the NodeEffect maintenance and combined reboot+maintenance flows.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use kube::core::ObjectMeta;
use tokio::sync::{Notify, broadcast};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::helpers::{Collector, make_dpu, make_dpu_reboot};
use crate::crds::dpunodemaintenances_generated::*;
use crate::crds::dpunodes_generated::*;
use crate::crds::dpus_generated::*;
use crate::error::DpfError;
use crate::repository::{
    DpfOperatorConfigRepository, DpuNodeMaintenanceRepository, DpuNodeRepository, DpuRepository,
    K8sConfigRepository,
};
use crate::sdk::{DpfSdkBuilder, HOLD_ANNOTATION, RESTART_ANNOTATION};
use crate::types::*;

const TEST_NS: &str = "maintenance-flow-ns";

/// Mock implementing DpuRepository (watch), DpuNodeRepository, and
/// DpuNodeMaintenanceRepository for maintenance flow tests.
#[derive(Clone)]
struct MaintenanceFlowMock {
    nodes: Arc<RwLock<BTreeMap<String, DPUNode>>>,
    maintenances: Arc<RwLock<BTreeMap<String, DPUNodeMaintenance>>>,
    dpu_tx: broadcast::Sender<DPU>,
    cancel: CancellationToken,
    watch_count: Arc<AtomicUsize>,
    watch_notify: Arc<Notify>,
}

impl MaintenanceFlowMock {
    fn new() -> Self {
        let (dpu_tx, _) = broadcast::channel(100);
        Self {
            nodes: Arc::default(),
            maintenances: Arc::default(),
            dpu_tx,
            cancel: CancellationToken::new(),
            watch_count: Arc::default(),
            watch_notify: Arc::new(Notify::new()),
        }
    }

    fn emit_dpu(&self, dpu: DPU) {
        let _ = self.dpu_tx.send(dpu);
    }

    fn insert_node(&self, node: &DPUNode) {
        let key = node.metadata.name.clone().unwrap_or_default();
        self.nodes.write().unwrap().insert(key, node.clone());
    }

    fn insert_maintenance(&self, m: &DPUNodeMaintenance) {
        let key = m.metadata.name.clone().unwrap_or_default();
        self.maintenances.write().unwrap().insert(key, m.clone());
    }

    fn get_maintenance(&self, name: &str) -> Option<DPUNodeMaintenance> {
        self.maintenances.read().unwrap().get(name).cloned()
    }

    async fn wait_for_watchers(&self, n: usize) {
        let res = timeout(Duration::from_secs(5), async {
            loop {
                if self.watch_count.load(Ordering::SeqCst) >= n {
                    return;
                }
                self.watch_notify.notified().await;
            }
        })
        .await;
        if res.is_err() {
            panic!(
                "Timed out waiting for {} watchers, got {}",
                n,
                self.watch_count.load(Ordering::SeqCst)
            );
        }
    }
}

#[async_trait]
impl DpuRepository for MaintenanceFlowMock {
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
        handler: F,
    ) -> impl Future<Output = ()> + Send + 'static
    where
        F: Fn(Arc<DPU>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        let rx = self.dpu_tx.subscribe();
        let cancel = self.cancel.clone();
        self.watch_count.fetch_add(1, Ordering::SeqCst);
        self.watch_notify.notify_waiters();
        async move {
            let stream =
                tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|r| async { r.ok() });
            tokio::pin!(stream);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    item = stream.next() => {
                        let Some(dpu) = item else { break };
                        let _ = handler(Arc::new(dpu)).await;
                    }
                }
            }
        }
    }
}

#[async_trait]
impl DpuNodeRepository for MaintenanceFlowMock {
    async fn get(&self, name: &str, _: &str) -> Result<Option<DPUNode>, DpfError> {
        Ok(self.nodes.read().unwrap().get(name).cloned())
    }
    async fn list(&self, _: &str) -> Result<Vec<DPUNode>, DpfError> {
        Ok(self.nodes.read().unwrap().values().cloned().collect())
    }
    async fn create(&self, node: &DPUNode) -> Result<DPUNode, DpfError> {
        self.insert_node(node);
        Ok(node.clone())
    }
    async fn patch(&self, name: &str, _: &str, patch: serde_json::Value) -> Result<(), DpfError> {
        if let Some(node) = self.nodes.write().unwrap().get_mut(name)
            && let Some(annos) = patch
                .pointer("/metadata/annotations")
                .and_then(|v| v.as_object())
        {
            let node_annos = node.metadata.annotations.get_or_insert_with(BTreeMap::new);
            for (k, v) in annos {
                if v.is_null() {
                    node_annos.remove(k);
                } else if let Some(s) = v.as_str() {
                    node_annos.insert(k.clone(), s.to_string());
                }
            }
        }
        Ok(())
    }
    async fn delete(&self, _: &str, _: &str) -> Result<(), DpfError> {
        Ok(())
    }
}

#[async_trait]
impl DpuNodeMaintenanceRepository for MaintenanceFlowMock {
    async fn get(&self, name: &str, _: &str) -> Result<Option<DPUNodeMaintenance>, DpfError> {
        Ok(self.maintenances.read().unwrap().get(name).cloned())
    }
    async fn patch(&self, name: &str, _: &str, patch: serde_json::Value) -> Result<(), DpfError> {
        if let Some(m) = self.maintenances.write().unwrap().get_mut(name)
            && let Some(annos) = patch
                .pointer("/metadata/annotations")
                .and_then(|v| v.as_object())
        {
            let m_annos = m.metadata.annotations.get_or_insert_with(BTreeMap::new);
            for (k, v) in annos {
                if v.is_null() {
                    m_annos.remove(k);
                } else if let Some(s) = v.as_str() {
                    m_annos.insert(k.clone(), s.to_string());
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl K8sConfigRepository for MaintenanceFlowMock {
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
impl DpfOperatorConfigRepository for MaintenanceFlowMock {
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
async fn test_node_effect_maintenance_then_ready() {
    let mock = MaintenanceFlowMock::new();

    let maint = DPUNodeMaintenance {
        metadata: ObjectMeta {
            name: Some("n1-hold".into()),
            namespace: Some(TEST_NS.into()),
            annotations: Some(BTreeMap::from([(HOLD_ANNOTATION.into(), "true".into())])),
            ..Default::default()
        },
        spec: DpuNodeMaintenanceSpec {
            dpu_node_name: "n1".into(),
            node_effect: None,
            requestor: None,
        },
        status: None,
    };
    mock.insert_maintenance(&maint);

    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .build_without_resources()
        .await
        .unwrap();

    let maint_events = Arc::new(Collector::<MaintenanceEvent>::default());
    let ready_events = Arc::new(Collector::<DpuReadyEvent>::default());

    let mc = maint_events.clone();
    let rc = ready_events.clone();

    let _watcher = sdk
        .watcher()
        .on_maintenance_needed(move |e| {
            let mc = mc.clone();
            async move {
                mc.push(e);
                Ok(())
            }
        })
        .on_dpu_ready(move |e| {
            let rc = rc.clone();
            async move {
                rc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();

    mock.wait_for_watchers(1).await;

    mock.emit_dpu(make_dpu(
        TEST_NS,
        "d1",
        "dev1",
        "n1",
        DpuStatusPhase::NodeEffect,
    ));

    maint_events.wait_for(1).await;
    {
        let me = maint_events.items.lock().unwrap();
        assert_eq!(me[0].dpu_name, "d1");
        assert_eq!(me[0].node_name, "n1");
    }

    let m = mock.get_maintenance("n1-hold").unwrap();
    assert_eq!(
        m.metadata
            .annotations
            .as_ref()
            .unwrap()
            .get(HOLD_ANNOTATION),
        Some(&"true".to_string())
    );

    sdk.release_maintenance_hold("n1").await.unwrap();

    let m = mock.get_maintenance("n1-hold").unwrap();
    assert_eq!(
        m.metadata
            .annotations
            .as_ref()
            .unwrap()
            .get(HOLD_ANNOTATION),
        Some(&"false".to_string())
    );

    mock.emit_dpu(make_dpu(TEST_NS, "d1", "dev1", "n1", DpuStatusPhase::Ready));

    ready_events.wait_for(1).await;
    assert_eq!(ready_events.items.lock().unwrap()[0].dpu_name, "d1");
}

#[tokio::test]
async fn test_reboot_then_node_effect_then_ready() {
    let mock = MaintenanceFlowMock::new();

    let node = DPUNode {
        metadata: ObjectMeta {
            name: Some("n1".into()),
            namespace: Some(TEST_NS.into()),
            annotations: Some(BTreeMap::from([(RESTART_ANNOTATION.into(), "true".into())])),
            ..Default::default()
        },
        spec: DpuNodeSpec {
            dpus: None,
            node_dms_address: None,
            node_reboot_method: None,
        },
        status: None,
    };
    mock.insert_node(&node);

    let maint = DPUNodeMaintenance {
        metadata: ObjectMeta {
            name: Some("n1-hold".into()),
            namespace: Some(TEST_NS.into()),
            annotations: Some(BTreeMap::from([(HOLD_ANNOTATION.into(), "true".into())])),
            ..Default::default()
        },
        spec: DpuNodeMaintenanceSpec {
            dpu_node_name: "n1".into(),
            node_effect: None,
            requestor: None,
        },
        status: None,
    };
    mock.insert_maintenance(&maint);

    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .build_without_resources()
        .await
        .unwrap();

    let reboot_events = Arc::new(Collector::<RebootRequiredEvent>::default());
    let maint_events = Arc::new(Collector::<MaintenanceEvent>::default());
    let ready_events = Arc::new(Collector::<DpuReadyEvent>::default());

    let rbe = reboot_events.clone();
    let mc = maint_events.clone();
    let rc = ready_events.clone();

    let _watcher = sdk
        .watcher()
        .on_reboot_required(move |e| {
            let rbe = rbe.clone();
            async move {
                rbe.push(e);
                Ok(())
            }
        })
        .on_maintenance_needed(move |e| {
            let mc = mc.clone();
            async move {
                mc.push(e);
                Ok(())
            }
        })
        .on_dpu_ready(move |e| {
            let rc = rc.clone();
            async move {
                rc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();

    mock.wait_for_watchers(1).await;

    mock.emit_dpu(make_dpu_reboot(TEST_NS, "d1", "dev1", "n1"));
    reboot_events.wait_for(1).await;
    assert_eq!(reboot_events.items.lock().unwrap()[0].dpu_name, "d1");

    assert!(sdk.is_reboot_required("n1").await.unwrap());

    sdk.reboot_complete("n1").await.unwrap();
    assert!(!sdk.is_reboot_required("n1").await.unwrap());

    mock.emit_dpu(make_dpu(
        TEST_NS,
        "d1",
        "dev1",
        "n1",
        DpuStatusPhase::NodeEffect,
    ));
    maint_events.wait_for(1).await;
    assert_eq!(maint_events.items.lock().unwrap()[0].node_name, "n1");

    let m = mock.get_maintenance("n1-hold").unwrap();
    assert_eq!(
        m.metadata
            .annotations
            .as_ref()
            .unwrap()
            .get(HOLD_ANNOTATION),
        Some(&"true".to_string())
    );

    sdk.release_maintenance_hold("n1").await.unwrap();
    let m = mock.get_maintenance("n1-hold").unwrap();
    assert_eq!(
        m.metadata
            .annotations
            .as_ref()
            .unwrap()
            .get(HOLD_ANNOTATION),
        Some(&"false".to_string())
    );

    mock.emit_dpu(make_dpu(TEST_NS, "d1", "dev1", "n1", DpuStatusPhase::Ready));
    ready_events.wait_for(1).await;
    assert_eq!(ready_events.items.lock().unwrap()[0].dpu_name, "d1");

    assert_eq!(reboot_events.len(), 1);
    assert_eq!(maint_events.len(), 1);
    assert_eq!(ready_events.len(), 1);
}
