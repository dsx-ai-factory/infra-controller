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

//! Tests for the full provisioning flow: watcher-driven reboot then ready.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use kube::core::ObjectMeta;
use tokio::sync::{Notify, broadcast};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::helpers::{Collector, make_dpu, make_dpu_reboot};
use crate::crds::dpunodes_generated::*;
use crate::crds::dpus_generated::*;
use crate::error::DpfError;
use crate::repository::{
    DpfOperatorConfigRepository, DpuNodeRepository, DpuRepository, K8sConfigRepository,
};
use crate::sdk::{DpfSdkBuilder, RESTART_ANNOTATION};
use crate::types::*;

const TEST_NS: &str = "sdk-provisioning-ns";

#[derive(Clone)]
struct ProvisioningFlowMock {
    nodes: Arc<RwLock<BTreeMap<String, DPUNode>>>,
    dpu_tx: broadcast::Sender<DPU>,
    cancel: CancellationToken,
    watch_count: Arc<std::sync::atomic::AtomicUsize>,
    watch_notify: Arc<Notify>,
}

impl ProvisioningFlowMock {
    fn new() -> Self {
        let (dpu_tx, _) = broadcast::channel(100);
        Self {
            nodes: Arc::default(),
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

    async fn wait_for_watchers(&self, n: usize) {
        let res = timeout(Duration::from_secs(5), async {
            loop {
                if self.watch_count.load(std::sync::atomic::Ordering::SeqCst) >= n {
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
                self.watch_count.load(std::sync::atomic::Ordering::SeqCst)
            );
        }
    }
}

#[async_trait]
impl DpuRepository for ProvisioningFlowMock {
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
        self.watch_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
impl DpuNodeRepository for ProvisioningFlowMock {
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
impl K8sConfigRepository for ProvisioningFlowMock {
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
impl DpfOperatorConfigRepository for ProvisioningFlowMock {
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
async fn test_provisioning_flow_reboot_then_ready() {
    let mock = ProvisioningFlowMock::new();

    // Set up a DPUNode with the reboot annotation
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

    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .build_without_resources()
        .await
        .unwrap();

    // Collect events
    let dpu_events = Arc::new(Collector::<DpuEvent>::default());
    let ready_events = Arc::new(Collector::<DpuReadyEvent>::default());
    let reboot_events = Arc::new(Collector::<RebootRequiredEvent>::default());

    let dc = dpu_events.clone();
    let re = ready_events.clone();
    let rbe = reboot_events.clone();

    let _watcher = sdk
        .watcher()
        .on_dpu_event(move |e| {
            dc.push(e);
            async { Ok(()) }
        })
        .on_dpu_ready(move |e| {
            re.push(e);
            async { Ok(()) }
        })
        .on_reboot_required(move |e| {
            let rbe = rbe.clone();
            async move {
                rbe.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();

    mock.wait_for_watchers(1).await;

    // Operator emits DPU in Rebooting phase with host_reboot_required
    mock.emit_dpu(make_dpu_reboot(TEST_NS, "d1", "dev1", "n1"));

    // Both dpu event and reboot callbacks should fire
    dpu_events.wait_for(1).await;
    reboot_events.wait_for(1).await;

    // Annotation should still be present
    let node = DpuNodeRepository::get(&mock, "n1", TEST_NS)
        .await
        .unwrap()
        .unwrap();
    assert!(
        node.metadata
            .annotations
            .as_ref()
            .unwrap()
            .contains_key(RESTART_ANNOTATION)
    );

    // Simulate the caller clearing the annotation after rebooting the host
    sdk.reboot_complete("n1").await.unwrap();

    // Annotation should be gone
    let node = DpuNodeRepository::get(&mock, "n1", TEST_NS)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !node
            .metadata
            .annotations
            .unwrap_or_default()
            .contains_key(RESTART_ANNOTATION)
    );

    // Operator progresses DPU to Ready
    mock.emit_dpu(make_dpu(TEST_NS, "d1", "dev1", "n1", DpuStatusPhase::Ready));

    ready_events.wait_for(1).await;
}
