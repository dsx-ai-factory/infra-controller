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

//! Tests for releasing the DPF maintenance hold annotation.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kube::core::ObjectMeta;

use crate::crds::dpunodemaintenances_generated::*;
use crate::error::DpfError;
use crate::repository::{
    DpfOperatorConfigRepository, DpuNodeMaintenanceRepository, K8sConfigRepository,
};
use crate::sdk::{DpfSdkBuilder, HOLD_ANNOTATION};

const TEST_NS: &str = "sdk-maintenance-ns";

#[derive(Clone, Default)]
struct MaintenanceHoldMock {
    maintenances: Arc<RwLock<BTreeMap<String, DPUNodeMaintenance>>>,
}

impl MaintenanceHoldMock {
    fn insert(&self, m: &DPUNodeMaintenance) {
        let key = m.metadata.name.clone().unwrap_or_default();
        self.maintenances.write().unwrap().insert(key, m.clone());
    }

    fn get(&self, name: &str) -> Option<DPUNodeMaintenance> {
        self.maintenances.read().unwrap().get(name).cloned()
    }
}

#[async_trait]
impl DpuNodeMaintenanceRepository for MaintenanceHoldMock {
    async fn get(&self, name: &str, _: &str) -> Result<Option<DPUNodeMaintenance>, DpfError> {
        Ok(self.maintenances.read().unwrap().get(name).cloned())
    }
    async fn patch(&self, name: &str, _: &str, patch: serde_json::Value) -> Result<(), DpfError> {
        let mut store = self.maintenances.write().unwrap();
        let m = store.get_mut(name).ok_or_else(|| {
            DpfError::KubeError(kube::Error::Api(Box::new(
                kube::core::Status::failure(&format!("{name} not found"), "NotFound")
                    .with_code(404),
            )))
        })?;
        if let Some(annos) = patch
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
impl K8sConfigRepository for MaintenanceHoldMock {
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
impl DpfOperatorConfigRepository for MaintenanceHoldMock {
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
async fn test_release_maintenance_hold_sets_annotation_false() {
    let mock = MaintenanceHoldMock::default();
    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .build_without_resources()
        .await
        .unwrap();

    // Pre-populate a DPUNodeMaintenance with hold annotation set to "true"
    let maint = DPUNodeMaintenance {
        metadata: ObjectMeta {
            name: Some("node-host-001-hold".into()),
            namespace: Some(TEST_NS.into()),
            annotations: Some(BTreeMap::from([(HOLD_ANNOTATION.into(), "true".into())])),
            ..Default::default()
        },
        spec: DpuNodeMaintenanceSpec {
            dpu_node_name: "node-host-001".into(),
            node_effect: None,
            requestor: None,
        },
        status: None,
    };
    mock.insert(&maint);

    // Verify hold is true
    let m = mock.get("node-host-001-hold").unwrap();
    assert_eq!(
        m.metadata
            .annotations
            .as_ref()
            .unwrap()
            .get(HOLD_ANNOTATION),
        Some(&"true".to_string())
    );

    // Release the maintenance hold
    sdk.release_maintenance_hold("node-host-001").await.unwrap();

    // Hold annotation should now be "false"
    let m = mock.get("node-host-001-hold").unwrap();
    assert_eq!(
        m.metadata
            .annotations
            .as_ref()
            .unwrap()
            .get(HOLD_ANNOTATION),
        Some(&"false".to_string())
    );
}

#[tokio::test]
async fn test_release_maintenance_hold_noop_when_cr_missing() {
    let mock = MaintenanceHoldMock::default();
    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .build_without_resources()
        .await
        .unwrap();

    // No DPUNodeMaintenance CR exists — release_maintenance_hold should succeed as a no-op
    let result = sdk.release_maintenance_hold("node-nonexistent").await;
    assert!(
        result.is_ok(),
        "expected Ok for missing CR, got: {result:?}"
    );
}
