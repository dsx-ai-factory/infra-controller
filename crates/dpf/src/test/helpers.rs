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

//! Shared test infrastructure for DPF tests.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use kube::core::ObjectMeta;
use tokio::sync::{Notify, broadcast};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::crds::dpus_generated::*;
use crate::error::DpfError;
use crate::repository::{DpfOperatorConfigRepository, DpuRepository, K8sConfigRepository};

pub(super) struct Collector<T> {
    pub(super) items: Mutex<Vec<T>>,
    notify: Notify,
}

impl<T> Default for Collector<T> {
    fn default() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            notify: Notify::new(),
        }
    }
}

impl<T: Clone> Collector<T> {
    pub(super) fn push(&self, item: T) {
        self.items.lock().unwrap().push(item);
        self.notify.notify_waiters();
    }

    pub(super) fn len(&self) -> usize {
        self.items.lock().unwrap().len()
    }

    pub(super) fn get(&self, i: usize) -> Option<T> {
        self.items.lock().unwrap().get(i).cloned()
    }

    pub(super) fn all(&self) -> Vec<T> {
        self.items.lock().unwrap().clone()
    }

    pub(super) async fn wait_for(&self, n: usize) {
        let res = timeout(Duration::from_secs(5), async {
            loop {
                if self.len() >= n {
                    return;
                }
                self.notify.notified().await;
            }
        })
        .await;
        if res.is_err() {
            panic!("Timed out waiting for {} items, got {}", n, self.len());
        }
    }
}

/// Minimal DpuRepository mock that emits DPUs via broadcast channel.
#[derive(Clone)]
pub(super) struct WatcherMock {
    dpu_tx: broadcast::Sender<DPU>,
    cancel: CancellationToken,
    watch_count: Arc<AtomicUsize>,
    watch_notify: Arc<Notify>,
}

impl WatcherMock {
    pub(super) fn new() -> Self {
        let (dpu_tx, _) = broadcast::channel(100);
        Self {
            dpu_tx,
            cancel: CancellationToken::new(),
            watch_count: Arc::default(),
            watch_notify: Arc::new(Notify::new()),
        }
    }

    pub(super) fn emit_dpu(&self, dpu: DPU) {
        let _ = self.dpu_tx.send(dpu);
    }

    pub(super) async fn wait_for_watchers(&self, n: usize) {
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

    #[allow(dead_code)]
    pub(super) async fn wait_for_receivers(&self, n: usize) {
        let res = timeout(Duration::from_secs(5), async {
            loop {
                if self.dpu_tx.receiver_count() == n {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if res.is_err() {
            panic!(
                "Timed out waiting for {} receivers, got {}",
                n,
                self.dpu_tx.receiver_count()
            );
        }
    }
}

#[async_trait]
impl DpuRepository for WatcherMock {
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

pub(super) fn make_status(phase: DpuStatusPhase) -> DpuStatus {
    DpuStatus {
        addresses: None,
        bf_cfg_file: None,
        bfb_file: None,
        bfb_version: None,
        conditions: None,
        dpf_version: None,
        dpu_install_interface: None,
        dpu_mode: None,
        firmware: None,
        observed_generation: None,
        pci_device: None,
        phase,
        post_provisioning_node_effect: None,
        required_reset: None,
        agent_last_startup_time: None,
        agent_status: None,
        dpu_type: None,
        operational_conditions: None,
        previous_phase: None,
        redfish_task_id: None,
        secure_boot: None,
        deployment_mode: None,
        hostless: None,
        identity_mode: None,
        outdated: None,
        reboot_status: None,
    }
}

pub(super) fn make_dpu(
    ns: &str,
    name: &str,
    device: &str,
    node: &str,
    phase: DpuStatusPhase,
) -> DPU {
    DPU {
        metadata: ObjectMeta {
            name: Some(name.into()),
            namespace: Some(ns.into()),
            ..Default::default()
        },
        spec: DpuSpec {
            bfb: Some("bfb".to_string()),
            bmc_ip: Some("10.0.0.100".into()),
            cluster: None,
            dpu_device_name: device.into(),
            dpu_flavor: "flavor".to_string(),
            dpu_node_name: node.into(),
            node_effect: DpuNodeEffect {
                apply_on_label_change: None,
                custom_action: None,
                custom_label: None,
                drain: None,
                force: None,
                hold: None,
                no_effect: None,
                node_maintenance_additional_requestors: None,
                taint: None,
            },
            pci_address: None,
            serial_number: "SN".into(),
            blue_field_software: None,
            secure_boot: None,
            astra_enabled: None,
        },
        status: Some(make_status(phase)),
    }
}

pub(super) fn make_dpu_reboot(ns: &str, name: &str, device: &str, node: &str) -> DPU {
    make_dpu(ns, name, device, node, DpuStatusPhase::Rebooting)
}

#[allow(dead_code)]
pub(super) fn make_dpu_labeled(
    ns: &str,
    name: &str,
    device: &str,
    node: &str,
    phase: DpuStatusPhase,
    mid: &str,
) -> DPU {
    let mut dpu = make_dpu(ns, name, device, node, phase);
    dpu.metadata.labels = Some(BTreeMap::from([("test/dpu-machine-id".into(), mid.into())]));
    dpu
}

/// Minimal K8sConfigRepository mock for tests that only need
/// `build_without_resources` / `initialize` (BMC secret creation).
pub(super) struct ConfigMock;

#[async_trait]
impl K8sConfigRepository for ConfigMock {
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
impl DpfOperatorConfigRepository for ConfigMock {
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
