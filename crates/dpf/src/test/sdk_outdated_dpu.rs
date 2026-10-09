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

//! `DpfSdk::is_dpu_outdated`, the single-DPU staleness check that gates work
//! which must not land on a DPU still awaiting reprovisioning.
//!
//! Every path that cannot produce a confident "this DPU is current" must report
//! `true`, because the caller's safe action is to do nothing.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dashmap::DashMap;
use k8s_openapi::api::apps::v1::Deployment;
use kube::core::ObjectMeta;

use crate::crds::dpfoperatorconfigs_generated::DPFOperatorConfig;
use crate::crds::dpudeployments_generated::{
    DPUDeployment, DpuDeploymentDpusDpuSets, DpuDeploymentDpusDpuSetsDpuNodeSelector,
};
use crate::crds::dpus_generated::{DPU, DpuStatusPhase};
use crate::crds::dpuservicetemplates_generated::DPUServiceTemplate;
use crate::error::DpfError;
use crate::repository::{
    DpfOperatorConfigRepository, DpuDeploymentRepository, DpuRepository,
    DpuServiceTemplateRepository, K8sConfigRepository,
};
use crate::sdk::{DpfSdkBuilder, ResourceLabeler};
use crate::types::{DPU_ENABLED_NODE_LABEL, DpuDeploymentType, DpuPhase};

const TEST_NS: &str = "test-namespace";
const DEPLOYMENT: &str = "test-deployment";
const FLAVOR: &str = "test-flavor";
const DPU_NAME: &str = "node-host-001-device-001";
const OWNED_BY_LABEL: &str = "svc.dpu.nvidia.com/owned-by-dpudeployment";
const BF3_LABEL: &str = "test.nvidia.com/bf3";
const GB200_LABEL: &str = "test.nvidia.com/bf3gb200";

#[derive(Default, Clone)]
struct OutdatedDpuMock {
    dpus: Arc<DashMap<String, DPU>>,
    deployments: Arc<DashMap<String, DPUDeployment>>,
    operator_config: Arc<DashMap<String, DPFOperatorConfig>>,
    controller_deployments: Arc<DashMap<String, Deployment>>,
    dpu_list_selectors: Arc<Mutex<Vec<Option<String>>>>,
}

impl OutdatedDpuMock {
    fn with(dpu: DPU, deployment: DPUDeployment) -> Self {
        let mock = Self::default();
        mock.dpus.insert(DPU_NAME.to_string(), dpu);
        mock.deployments.insert(DEPLOYMENT.to_string(), deployment);
        mock
    }
}

#[async_trait]
impl DpuRepository for OutdatedDpuMock {
    async fn get(&self, name: &str, _ns: &str) -> Result<Option<DPU>, DpfError> {
        Ok(self.dpus.get(name).map(|dpu| dpu.clone()))
    }
    async fn list(&self, _ns: &str, selector: Option<&str>) -> Result<Vec<DPU>, DpfError> {
        self.dpu_list_selectors
            .lock()
            .unwrap()
            .push(selector.map(str::to_string));
        Ok(self.dpus.iter().map(|e| e.value().clone()).collect())
    }
    async fn patch_status(
        &self,
        _name: &str,
        _ns: &str,
        _patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete(&self, name: &str, _ns: &str) -> Result<(), DpfError> {
        self.dpus.remove(name);
        Ok(())
    }
    async fn delete_if_uid(&self, name: &str, ns: &str, uid: &str) -> Result<(), DpfError> {
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
        DpuRepository::delete(self, name, ns).await
    }
    fn watch<F, Fut>(
        &self,
        _ns: &str,
        _selector: Option<&str>,
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
impl DpfOperatorConfigRepository for OutdatedDpuMock {
    async fn get_controller_deployment(
        &self,
        name: &str,
        _namespace: &str,
    ) -> Result<Option<Deployment>, DpfError> {
        Ok(self
            .controller_deployments
            .get(name)
            .map(|deployment| deployment.clone()))
    }

    async fn get(&self, name: &str, _ns: &str) -> Result<Option<DPFOperatorConfig>, DpfError> {
        Ok(self.operator_config.get(name).map(|c| c.clone()))
    }
    async fn patch(&self, _: &str, _: &str, _: serde_json::Value) -> Result<(), DpfError> {
        Ok(())
    }
}

#[async_trait]
impl DpuDeploymentRepository for OutdatedDpuMock {
    async fn get(&self, name: &str, _ns: &str) -> Result<Option<DPUDeployment>, DpfError> {
        Ok(self.deployments.get(name).map(|d| d.clone()))
    }
    async fn list(&self, _ns: &str) -> Result<Vec<DPUDeployment>, DpfError> {
        Ok(self.deployments.iter().map(|e| e.value().clone()).collect())
    }
    async fn apply(&self, d: &DPUDeployment) -> Result<DPUDeployment, DpfError> {
        Ok(d.clone())
    }
    async fn patch(
        &self,
        _name: &str,
        _ns: &str,
        _patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        Ok(())
    }
    async fn delete(&self, _name: &str, _ns: &str) -> Result<(), DpfError> {
        Ok(())
    }
}

/// Required by `build_without_resources`; nothing here reads config or secrets.
#[async_trait]
impl K8sConfigRepository for OutdatedDpuMock {
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
impl DpuServiceTemplateRepository for OutdatedDpuMock {
    async fn get(&self, _name: &str, _ns: &str) -> Result<Option<DPUServiceTemplate>, DpfError> {
        Ok(None)
    }
    async fn list(&self, _ns: &str) -> Result<Vec<DPUServiceTemplate>, DpfError> {
        Ok(vec![])
    }
    async fn apply(&self, t: &DPUServiceTemplate) -> Result<DPUServiceTemplate, DpfError> {
        Ok(t.clone())
    }
}

/// A DPU owned by [`DEPLOYMENT`], built through serde so the literal names only
/// the fields these tests care about.
fn dpu(
    bfb: Option<&str>,
    blue_field_software: Option<&str>,
    installed_bfb_file: Option<&str>,
    flavor: &str,
) -> DPU {
    let spec = serde_json::json!({
        "bfb": bfb,
        "blueFieldSoftware": blue_field_software,
        "dpuDeviceName": "device-001",
        "dpuFlavor": flavor,
        "dpuNodeName": "node-host-001",
        "nodeEffect": {},
        "serialNumber": "SN123",
    });
    let status = serde_json::json!({
        "phase": "Ready",
        "bfbFile": installed_bfb_file,
    });

    DPU {
        metadata: ObjectMeta {
            name: Some(DPU_NAME.to_string()),
            namespace: Some(TEST_NS.to_string()),
            labels: Some(BTreeMap::from([(
                OWNED_BY_LABEL.to_string(),
                format!("{TEST_NS}_{DEPLOYMENT}"),
            )])),
            ..Default::default()
        },
        spec: serde_json::from_value(spec).expect("valid DpuSpec"),
        status: Some(serde_json::from_value(status).expect("valid DpuStatus")),
    }
}

/// `ready` controls whether `DPUSetsReconciled` is `True` at the current
/// generation, which is what [`dpu_deployment_is_ready`] requires.
fn deployment(bfb: Option<&str>, blue_field_software: Option<&str>, ready: bool) -> DPUDeployment {
    let spec = serde_json::json!({
        "dpus": {
            "bfb": bfb,
            "blueFieldSoftware": blue_field_software,
            "flavor": FLAVOR,
            "dpuSetStrategy": { "type": "OnDelete" },
            "nodeEffect": {},
        },
        "services": {},
        "serviceChains": {
            "switches": [],
            "upgradePolicy": { "applyNodeEffect": true },
        },
    });
    let status = serde_json::json!({
        "conditions": [{
            "type": "DPUSetsReconciled",
            "status": if ready { "True" } else { "False" },
            "observedGeneration": 1,
            "lastTransitionTime": "2026-01-01T00:00:00Z",
            "reason": "Test",
        }],
    });

    DPUDeployment {
        metadata: ObjectMeta {
            name: Some(DEPLOYMENT.to_string()),
            namespace: Some(TEST_NS.to_string()),
            generation: Some(1),
            ..Default::default()
        },
        spec: serde_json::from_value(spec).expect("valid DpuDeploymentSpec"),
        status: Some(serde_json::from_value(status).expect("valid DpuDeploymentStatus")),
    }
}

fn template_deployment(
    bfb: Option<&str>,
    blue_field_software: Option<&str>,
    ready: bool,
) -> DPUDeployment {
    let mut deployment = deployment(bfb, blue_field_software, ready);
    deployment.spec.dpus.flavor = None;
    deployment.spec.dpus.flavor_template = Some("astra-flavor-template".to_string());
    deployment
}

/// Supplies the two BF3 deployment selectors used by the conformance test.
struct DeploymentTypeLabeler;

impl ResourceLabeler for DeploymentTypeLabeler {
    fn node_labels_for_deployment_type(
        &self,
        deployment_type: DpuDeploymentType,
    ) -> Result<BTreeMap<String, String>, DpfError> {
        let deployment_label = match deployment_type {
            DpuDeploymentType::Bf3 => BF3_LABEL,
            DpuDeploymentType::Bf3Gb200 => GB200_LABEL,
            other => {
                return Err(DpfError::ConfigError(format!(
                    "no test deployment configured for {other:?}"
                )));
            }
        };

        Ok(BTreeMap::from([
            (DPU_ENABLED_NODE_LABEL.to_string(), "true".to_string()),
            (deployment_label.to_string(), "true".to_string()),
        ]))
    }
}

/// Add the DPUSet selector that identifies a deployment type.
fn deployment_with_selector(
    bfb: Option<&str>,
    ready: bool,
    deployment_type: DpuDeploymentType,
) -> DPUDeployment {
    deployment_with_selector_for_fixture(deployment(bfb, None, ready), deployment_type)
}

/// Adds the selector for a deployment type to an otherwise complete fixture.
fn deployment_with_selector_for_fixture(
    mut deployment: DPUDeployment,
    deployment_type: DpuDeploymentType,
) -> DPUDeployment {
    let match_labels = DeploymentTypeLabeler
        .node_labels_for_deployment_type(deployment_type)
        .expect("test deployment selector");
    deployment.spec.dpus.dpu_sets = Some(vec![DpuDeploymentDpusDpuSets {
        dpu_annotations: None,
        dpu_selector: None,
        name_suffix: "default".to_string(),
        dpu_node_selector: Some(DpuDeploymentDpusDpuSetsDpuNodeSelector {
            match_expressions: None,
            match_labels: Some(match_labels),
        }),
        dpu_cluster_selector: None,
        dpu_device_selector: None,
        node_selector: None,
    }]);
    deployment
}

/// Replace the owning deployment label on a DPU fixture.
fn set_dpu_owner(mut dpu: DPU, deployment_name: &str) -> DPU {
    dpu.metadata.labels = Some(BTreeMap::from([(
        OWNED_BY_LABEL.to_string(),
        format!("{TEST_NS}_{deployment_name}"),
    )]));
    dpu
}

async fn phase_for_deployment_type(mock: OutdatedDpuMock) -> Result<Option<DpuPhase>, DpfError> {
    let phases = DpfSdkBuilder::new(mock, TEST_NS, String::new())
        .with_labeler(DeploymentTypeLabeler)
        .build_without_resources()
        .await
        .expect("sdk")
        .get_dpu_phases_for_deployment_type(
            &["001".to_string()],
            "node-host-001",
            DpuDeploymentType::Bf3Gb200,
        )
        .await?;

    Ok(phases.and_then(|mut phases| phases.remove("001")))
}

async fn is_outdated(mock: OutdatedDpuMock) -> Result<bool, DpfError> {
    DpfSdkBuilder::new(mock, TEST_NS, String::new())
        .build_without_resources()
        .await
        .expect("sdk")
        .is_dpu_outdated(DPU_NAME)
        .await
}

/// A phase read scoped to one deployment accepts work still running on the target,
/// while Ready also requires the target flavor and provisioning source.
#[tokio::test]
async fn a_dpu_phase_must_belong_to_the_requested_deployment_type() {
    struct Case {
        name: &'static str,
        owner: &'static str,
        flavor: &'static str,
        phase: DpuStatusPhase,
        installed_bfb_file: Option<&'static str>,
        deployment_ready: bool,
        expected: Option<DpuPhase>,
    }

    let cases = [
        Case {
            name: "source deployment still owns the DPU",
            owner: "source-deployment",
            flavor: FLAVOR,
            phase: DpuStatusPhase::Ready,
            installed_bfb_file: Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            deployment_ready: true,
            expected: None,
        },
        Case {
            name: "target deployment owns a DPU still installing",
            owner: DEPLOYMENT,
            flavor: FLAVOR,
            phase: DpuStatusPhase::OsInstalling,
            installed_bfb_file: None,
            deployment_ready: true,
            expected: Some(DpuPhase::Provisioning("OsInstalling".to_string())),
        },
        Case {
            name: "target deployment has not reconciled its DPU sets",
            owner: DEPLOYMENT,
            flavor: FLAVOR,
            phase: DpuStatusPhase::Ready,
            installed_bfb_file: Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            deployment_ready: false,
            expected: None,
        },
        Case {
            name: "target deployment owns a matching DPU",
            owner: DEPLOYMENT,
            flavor: FLAVOR,
            phase: DpuStatusPhase::Ready,
            installed_bfb_file: Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            deployment_ready: true,
            expected: Some(DpuPhase::Ready),
        },
    ];

    for case in cases {
        let mut dpu = set_dpu_owner(
            dpu(
                Some("bf-bundle-abc"),
                None,
                case.installed_bfb_file,
                case.flavor,
            ),
            case.owner,
        );
        dpu.status.as_mut().expect("DPU status").phase = case.phase;
        let deployment = deployment_with_selector(
            Some("bf-bundle-abc"),
            case.deployment_ready,
            DpuDeploymentType::Bf3Gb200,
        );

        assert_eq!(
            phase_for_deployment_type(OutdatedDpuMock::with(dpu, deployment))
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", case.name)),
            case.expected,
            "{}",
            case.name
        );
    }
}

/// A Ready DPU owned by the target cannot converge without another deletion,
/// so configuration drift must be reported instead of treated as recreation.
#[tokio::test]
async fn a_ready_target_dpu_with_configuration_drift_is_an_error() {
    let dpu = dpu(
        Some("bf-bundle-abc"),
        None,
        Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        "source-flavor",
    );
    let deployment =
        deployment_with_selector(Some("bf-bundle-abc"), true, DpuDeploymentType::Bf3Gb200);

    let error = phase_for_deployment_type(OutdatedDpuMock::with(dpu, deployment))
        .await
        .expect_err("Ready configuration drift must be visible");
    assert!(matches!(error, DpfError::InvalidState(_)));
}

/// A rendered DPUFlavorTemplate cannot be compared with the flavor name on a
/// DPU CR, so migration conformance reports a visible configuration error.
#[tokio::test]
async fn a_deployment_type_phase_rejects_a_flavor_template() {
    let dpu = dpu(
        Some("bf-bundle-abc"),
        None,
        Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        FLAVOR,
    );
    let mut deployment = template_deployment(Some("bf-bundle-abc"), None, true);
    deployment = deployment_with_selector_for_fixture(deployment, DpuDeploymentType::Bf3Gb200);

    let error = phase_for_deployment_type(OutdatedDpuMock::with(dpu, deployment))
        .await
        .expect_err("DPUFlavorTemplate must not produce a confident phase");
    assert!(matches!(error, DpfError::InvalidState(_)));
}

/// A deployment scoped phase read requires one unambiguous deployment owner.
#[tokio::test]
async fn a_deployment_type_phase_requires_exactly_one_deployment() {
    for (name, deployment_count, expected_message) in [
        ("no matching deployment", 0, "no DPUDeployment selects"),
        (
            "multiple matching deployments",
            2,
            "multiple DPUDeployments select",
        ),
    ] {
        let mock = OutdatedDpuMock::default();
        for index in 0..deployment_count {
            let mut deployment =
                deployment_with_selector(Some("bf-bundle-abc"), true, DpuDeploymentType::Bf3Gb200);
            let deployment_name = format!("target-deployment-{index}");
            deployment.metadata.name = Some(deployment_name.clone());
            mock.deployments.insert(deployment_name, deployment);
        }

        let error = phase_for_deployment_type(mock).await.expect_err(name);
        assert!(
            matches!(&error, DpfError::InvalidState(message) if message.contains(expected_message)),
            "{name}: {error}"
        );
    }
}

/// A deployment phase read asks Kubernetes only for DPUs owned by the target
/// deployment while retaining the per-resource ownership check.
#[tokio::test]
async fn a_deployment_type_phase_scopes_the_dpu_list_to_its_owner() {
    let dpu = set_dpu_owner(
        dpu(
            Some("bf-bundle-abc"),
            None,
            Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            FLAVOR,
        ),
        DEPLOYMENT,
    );
    let deployment =
        deployment_with_selector(Some("bf-bundle-abc"), true, DpuDeploymentType::Bf3Gb200);
    let mock = OutdatedDpuMock::with(dpu, deployment);

    phase_for_deployment_type(mock.clone())
        .await
        .expect("deployment phase");

    assert_eq!(
        *mock.dpu_list_selectors.lock().unwrap(),
        vec![Some(format!("{OWNED_BY_LABEL}={TEST_NS}_{DEPLOYMENT}"))]
    );
}

/// Retrying a deployment migration removes a DPU owned by the source but
/// preserves a replacement with the same name once the target owns it.
#[tokio::test]
async fn deployment_migration_deletion_preserves_target_replacements() {
    let mock = OutdatedDpuMock::default();
    let mut source_deployment =
        deployment_with_selector(Some("bf-bundle-abc"), true, DpuDeploymentType::Bf3);
    source_deployment.metadata.name = Some("source-deployment".to_string());
    let target_deployment =
        deployment_with_selector(Some("bf-bundle-abc"), true, DpuDeploymentType::Bf3Gb200);
    mock.deployments
        .insert("source-deployment".to_string(), source_deployment);
    mock.deployments
        .insert(DEPLOYMENT.to_string(), target_deployment);

    let mut source_dpu = set_dpu_owner(
        dpu(
            Some("bf-bundle-abc"),
            None,
            Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            FLAVOR,
        ),
        "source-deployment",
    );
    source_dpu.metadata.uid = Some("source-uid".to_string());
    mock.dpus.insert(DPU_NAME.to_string(), source_dpu);

    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .with_labeler(DeploymentTypeLabeler)
        .build_without_resources()
        .await
        .expect("sdk");
    sdk.delete_source_dpus_for_deployment_migration(
        &["001".to_string()],
        "node-host-001",
        DpuDeploymentType::Bf3,
        DpuDeploymentType::Bf3Gb200,
    )
    .await
    .expect("source DPU deletion");
    assert!(mock.dpus.get(DPU_NAME).is_none());

    let mut target_dpu = set_dpu_owner(
        dpu(
            Some("bf-bundle-abc"),
            None,
            Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            FLAVOR,
        ),
        DEPLOYMENT,
    );
    target_dpu.metadata.uid = Some("target-uid".to_string());
    mock.dpus.insert(DPU_NAME.to_string(), target_dpu);

    sdk.delete_source_dpus_for_deployment_migration(
        &["001".to_string()],
        "node-host-001",
        DpuDeploymentType::Bf3,
        DpuDeploymentType::Bf3Gb200,
    )
    .await
    .expect("migration retry");

    let replacement = mock
        .dpus
        .get(DPU_NAME)
        .expect("target replacement must be preserved");
    assert_eq!(replacement.metadata.uid.as_deref(), Some("target-uid"));
}

/// Both provisioning sources, matching and drifted. A DPUDeployment declares
/// either a BFB or a BlueFieldSoftware CR, and the two are compared differently:
/// a BFB against the image actually installed, BlueFieldSoftware against the CR
/// name the DPU was created with.
#[tokio::test]
async fn a_dpu_is_current_only_while_it_matches_its_declared_provisioning_source() {
    let cases: [(&str, DPU, DPUDeployment, bool); 4] = [
        (
            "BFB matches the installed image",
            dpu(
                Some("bf-bundle-abc"),
                None,
                Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
                FLAVOR,
            ),
            deployment(Some("bf-bundle-abc"), None, true),
            false,
        ),
        (
            "BFB moved on from the installed image",
            dpu(
                Some("bf-bundle-old"),
                None,
                Some("/bfb/test-namespace-bf-bundle-old.bfb"),
                FLAVOR,
            ),
            deployment(Some("bf-bundle-new"), None, true),
            true,
        ),
        (
            "BlueFieldSoftware matches the DPU's spec",
            dpu(None, Some("bf-software-abc"), None, FLAVOR),
            deployment(None, Some("bf-software-abc"), true),
            false,
        ),
        (
            "BlueFieldSoftware moved on from the DPU's spec",
            dpu(None, Some("bf-software-old"), None, FLAVOR),
            deployment(None, Some("bf-software-new"), true),
            true,
        ),
    ];

    for (name, dpu, deployment, expected_outdated) in cases {
        let outdated = is_outdated(OutdatedDpuMock::with(dpu, deployment))
            .await
            .unwrap_or_else(|error| panic!("{name}: evaluation failed: {error}"));
        assert_eq!(outdated, expected_outdated, "{name}");
    }
}

#[tokio::test]
async fn a_dpu_whose_flavor_drifted_is_outdated() {
    let mock = OutdatedDpuMock::with(
        dpu(
            Some("bf-bundle-abc"),
            None,
            Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            "some-other-flavor",
        ),
        deployment(Some("bf-bundle-abc"), None, true),
    );

    assert!(is_outdated(mock).await.expect("evaluated"));
}

#[tokio::test]
async fn a_template_deployment_does_not_compare_its_template_name_with_dpu_flavor() {
    let dpu = dpu(
        None,
        Some("bf-software-abc"),
        None,
        "per-dpu-rendered-flavor",
    );
    let deployment = template_deployment(None, Some("bf-software-abc"), true);

    // is_dpu_outdated and find_outdated_dpus_dpf share dpu_comparison: a
    // DPUFlavorTemplate name is not comparable with its generated DPUFlavor.
    assert!(
        !is_outdated(OutdatedDpuMock::with(dpu, deployment))
            .await
            .expect("template deployment can be evaluated")
    );
}

#[tokio::test]
async fn an_unready_deployment_leaves_the_dpu_outdated() {
    // Matches on every field, so only the deployment's readiness decides. An
    // unready deployment is still settling and its declared state is not yet
    // authoritative.
    let mock = OutdatedDpuMock::with(
        dpu(
            Some("bf-bundle-abc"),
            None,
            Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            FLAVOR,
        ),
        deployment(Some("bf-bundle-abc"), None, false),
    );

    assert!(is_outdated(mock).await.expect("evaluated"));
}

#[tokio::test]
async fn a_deployment_declaring_no_provisioning_source_leaves_the_dpu_outdated() {
    // Neither bfb nor blueFieldSoftware violates the DPU CRD's
    // `has(self.bfb) != has(self.blueFieldSoftware)` rule, so the comparison is
    // inconclusive. `find_outdated_dpus_dpf` skips this case; here it must not
    // read as "up to date".
    let mock = OutdatedDpuMock::with(
        dpu(Some("bf-bundle-abc"), None, None, FLAVOR),
        deployment(None, None, true),
    );

    assert!(is_outdated(mock).await.expect("evaluated"));
}

#[tokio::test]
async fn a_deployment_declaring_both_provisioning_sources_leaves_the_dpu_outdated() {
    let mock = OutdatedDpuMock::with(
        dpu(Some("bf-bundle-abc"), None, None, FLAVOR),
        deployment(Some("bf-bundle-abc"), Some("bf-software-abc"), true),
    );

    assert!(is_outdated(mock).await.expect("evaluated"));
}

#[tokio::test]
async fn a_missing_dpu_is_an_error_rather_than_a_verdict() {
    // Nothing seeded. Reporting `false` here would release a hold for a DPU
    // that cannot be inspected; reporting `true` would silently stall. The
    // caller needs to tell this apart from a real answer.
    let mock = OutdatedDpuMock::default();

    assert!(is_outdated(mock).await.is_err());
}

#[tokio::test]
async fn a_dpu_without_an_owner_label_is_an_error() {
    let mut orphan = dpu(
        Some("bf-bundle-abc"),
        None,
        Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        FLAVOR,
    );
    orphan.metadata.labels = None;
    let mock = OutdatedDpuMock::with(orphan, deployment(Some("bf-bundle-abc"), None, true));

    assert!(is_outdated(mock).await.is_err());
}

/// The operator reports current pre-upgrade validation failure before component
/// reconciliation, so aggregate readiness cannot gate recovery from stale DPUs.
fn upgrade_pending_mock(dpu: DPU, deployment: DPUDeployment) -> OutdatedDpuMock {
    let mock = OutdatedDpuMock::with(dpu, deployment);
    let config = serde_json::from_value(serde_json::json!({
        "apiVersion": "operator.dpu.nvidia.com/v1alpha1",
        "kind": "DPFOperatorConfig",
        "metadata": { "name": "dpfoperatorconfig", "generation": 2 },
        "spec": { "deploymentMode": "zero-trust", "provisioningController": {} },
        "status": { "observedGeneration": 2, "version": "v26.4.0", "conditions": [{
            "type": "Ready", "status": "False", "observedGeneration": 2,
            "lastTransitionTime": "2026-01-01T00:00:00Z",
            "reason": "Pending",
        }, {
            "type": "PreUpgradeValidationReady", "status": "False", "observedGeneration": 2,
            "lastTransitionTime": "2026-01-01T00:00:00Z",
            "reason": "Error",
        }, {
            "type": "ImagePullSecretsReconciled", "status": "True", "observedGeneration": 2,
            "lastTransitionTime": "2026-01-01T00:00:00Z", "reason": "Success",
        }, {
            "type": "SystemComponentsReconciled", "status": "True", "observedGeneration": 2,
            "lastTransitionTime": "2026-01-01T00:00:00Z", "reason": "Success",
        }, {
            "type": "SystemComponentsReady", "status": "False", "observedGeneration": 2,
            "lastTransitionTime": "2026-01-01T00:00:00Z", "reason": "Error",
        }] },
    }))
    .expect("valid operator config");
    mock.operator_config
        .insert("dpfoperatorconfig".to_string(), config);
    for name in [
        "dpf-provisioning-controller-manager",
        "dpuservice-controller-manager",
    ] {
        let deployment = serde_json::from_value(serde_json::json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": name, "namespace": TEST_NS, "generation": 1,
                "labels": {"operator.dpu.nvidia.com/dpf-version": "v26.4.0"}},
            "spec": {"replicas": 1, "selector": {"matchLabels": {"app": name}},
                "template": {"metadata": {"labels": {"app": name}},
                    "spec": {"containers": [{"name": "manager", "image": "old-dpf"}]}}},
            "status": {"observedGeneration": 1, "replicas": 1,
                "readyReplicas": 1, "availableReplicas": 1}
        }))
        .unwrap();
        mock.controller_deployments
            .insert(name.to_string(), deployment);
    }
    mock
}

#[tokio::test]
async fn outdated_scan_requires_a_current_operator_or_upgrade_blocker() {
    let stale = dpu(
        Some("bf-bundle-abc"),
        None,
        Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        "old-flavor",
    );
    let desired = deployment(Some("bf-bundle-abc"), None, true);
    let mock = upgrade_pending_mock(stale.clone(), desired.clone());
    let blocked = mock
        .operator_config
        .get("dpfoperatorconfig")
        .unwrap()
        .clone();
    let mut ready = blocked.clone();
    let conditions = ready.status.as_mut().unwrap().conditions.as_mut().unwrap();
    conditions[0].status = "True".to_string();
    conditions.truncate(1);
    let mut unrelated = blocked.clone();
    unrelated
        .status
        .as_mut()
        .unwrap()
        .conditions
        .as_mut()
        .unwrap()
        .truncate(1);
    let mut stale_ready = blocked.clone();
    stale_ready
        .status
        .as_mut()
        .unwrap()
        .conditions
        .as_mut()
        .unwrap()[0]
        .observed_generation = Some(1);
    let mut stale_validation = blocked.clone();
    stale_validation
        .status
        .as_mut()
        .unwrap()
        .conditions
        .as_mut()
        .unwrap()[1]
        .observed_generation = Some(1);
    let mut unknown = blocked.clone();
    unknown
        .status
        .as_mut()
        .unwrap()
        .conditions
        .as_mut()
        .unwrap()[0]
        .status = "Unknown".to_string();
    let mut deleting = ready.clone();
    deleting.metadata.deletion_timestamp =
        Some(serde_json::from_value(serde_json::json!("2026-01-01T00:00:00Z")).unwrap());
    let mut no_status = blocked.clone();
    no_status.status = None;
    let mut no_generation = blocked.clone();
    no_generation.metadata.generation = None;

    let mut cases = vec![
        ("ready operator", Some(ready), 1),
        (
            "upgrade blocker without targetVersion",
            Some(blocked.clone()),
            1,
        ),
        ("absent operator", None, 0),
        ("unready outside pre-upgrade validation", Some(unrelated), 0),
        ("stale aggregate readiness", Some(stale_ready), 0),
        ("stale upgrade validation", Some(stale_validation), 0),
        ("unknown aggregate readiness", Some(unknown), 0),
        ("deleting operator", Some(deleting), 0),
        ("missing operator status", Some(no_status), 0),
        ("missing operator generation", Some(no_generation), 0),
    ];
    for (name, path, value) in [
        (
            "stale operator status",
            "/status/observedGeneration",
            serde_json::json!(1),
        ),
        (
            "new operator",
            "/status/observedGeneration",
            serde_json::json!(0),
        ),
        (
            "missing installed version",
            "/status/version",
            serde_json::Value::Null,
        ),
        (
            "empty installed version",
            "/status/version",
            serde_json::json!(""),
        ),
        (
            "unrecognized validation failure",
            "/status/conditions/1/reason",
            serde_json::json!("Failure"),
        ),
        (
            "unknown validation",
            "/status/conditions/1/status",
            serde_json::json!("Unknown"),
        ),
        (
            "validation succeeded",
            "/status/conditions/1/status",
            serde_json::json!("True"),
        ),
        (
            "component failure outside expected upgrade readiness",
            "/status/conditions/4/reason",
            serde_json::json!("Failure"),
        ),
        (
            "unknown component",
            "/status/conditions/4/status",
            serde_json::json!("Unknown"),
        ),
        (
            "stale component readiness",
            "/status/conditions/4/observedGeneration",
            serde_json::json!(1),
        ),
        (
            "failed component reconciliation",
            "/status/conditions/3/status",
            serde_json::json!("False"),
        ),
    ] {
        let mut config = serde_json::to_value(&blocked).unwrap();
        *config.pointer_mut(path).unwrap() = value;
        cases.push((name, Some(serde_json::from_value(config).unwrap()), 0));
    }
    for (name, index) in [
        ("missing component readiness", 4),
        ("missing component reconciliation", 3),
        ("missing image-pull-secret reconciliation", 2),
    ] {
        let mut config = blocked.clone();
        config
            .status
            .as_mut()
            .unwrap()
            .conditions
            .as_mut()
            .unwrap()
            .remove(index);
        cases.push((name, Some(config), 0));
    }
    let mut upgraded = serde_json::to_value(&blocked).unwrap();
    upgraded["status"]["targetVersion"] = serde_json::json!("v26.4.0");
    cases.push((
        "equal installed and target versions",
        Some(serde_json::from_value(upgraded.clone()).unwrap()),
        0,
    ));
    upgraded["status"]["targetVersion"] = serde_json::json!("v26.8.0");
    cases.push((
        "explicit target version upgrade",
        Some(serde_json::from_value(upgraded).unwrap()),
        1,
    ));
    let mut paused = serde_json::to_value(&blocked).unwrap();
    paused["spec"]["overrides"] = serde_json::json!({ "paused": true });
    cases.push((
        "paused operator",
        Some(serde_json::from_value(paused).unwrap()),
        0,
    ));
    let mut other_failure = blocked;
    let mut extra = other_failure
        .status
        .as_ref()
        .unwrap()
        .conditions
        .as_ref()
        .unwrap()[4]
        .clone();
    extra.type_ = "CATrustBundleReady".to_string();
    extra.status = "False".to_string();
    other_failure
        .status
        .as_mut()
        .unwrap()
        .conditions
        .as_mut()
        .unwrap()
        .push(extra);
    cases.push(("other current validation failure", Some(other_failure), 0));

    for (name, config, expected_count) in cases.into_boxed_slice() {
        let mock = upgrade_pending_mock(stale.clone(), desired.clone());
        mock.operator_config.clear();
        if let Some(config) = config {
            mock.operator_config
                .insert("dpfoperatorconfig".to_string(), config);
        }
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
            .build_without_resources()
            .await
            .expect(name);
        assert_eq!(
            sdk.find_outdated_dpus_dpf(None).await.unwrap().len(),
            expected_count,
            "{name}"
        );
        if expected_count == 0 {
            assert!(mock.dpu_list_selectors.lock().unwrap().is_empty(), "{name}");
        }
    }
}

#[tokio::test]
async fn controller_workload_reads_require_backend_support() {
    let result = DpfOperatorConfigRepository::get_controller_deployment(
        &super::helpers::ConfigMock,
        "dpf-provisioning-controller-manager",
        TEST_NS,
    )
    .await;
    assert!(matches!(result, Err(DpfError::ConfigError(_))));
}

#[tokio::test]
async fn pending_upgrade_requires_available_replacement_controllers() {
    let stale = dpu(
        Some("bf-bundle-abc"),
        None,
        Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        "old-flavor",
    );
    let desired = deployment(Some("bf-bundle-abc"), None, true);
    for (name, path, value) in [
        (
            "old version with available controllers",
            "",
            serde_json::Value::Null,
        ),
        ("missing controller", "missing", serde_json::Value::Null),
        (
            "scaled down controller",
            "/spec/replicas",
            serde_json::json!(0),
        ),
        (
            "stale workload status",
            "/status/observedGeneration",
            serde_json::json!(0),
        ),
        (
            "no controller replicas",
            "/status/replicas",
            serde_json::json!(0),
        ),
        (
            "controller replicas not ready",
            "/status/readyReplicas",
            serde_json::json!(0),
        ),
        (
            "controller replicas unavailable",
            "/status/availableReplicas",
            serde_json::json!(0),
        ),
        (
            "missing workload status",
            "/status",
            serde_json::Value::Null,
        ),
        (
            "deleting controller",
            "/metadata/deletionTimestamp",
            serde_json::json!("2026-01-01T00:00:00Z"),
        ),
    ] {
        for controller in [
            "dpf-provisioning-controller-manager",
            "dpuservice-controller-manager",
        ] {
            let mock = upgrade_pending_mock(stale.clone(), desired.clone());
            if path == "missing" {
                mock.controller_deployments.remove(controller);
            } else if !path.is_empty() {
                let mut workload = serde_json::to_value(
                    mock.controller_deployments.get(controller).unwrap().value(),
                )
                .unwrap();
                if path == "/metadata/deletionTimestamp" {
                    workload["metadata"]["deletionTimestamp"] = value.clone();
                } else {
                    *workload.pointer_mut(path).unwrap() = value.clone();
                }
                mock.controller_deployments.insert(
                    controller.to_string(),
                    serde_json::from_value(workload).unwrap(),
                );
            }
            let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
                .build_without_resources()
                .await
                .unwrap();
            assert_eq!(
                sdk.find_outdated_dpus_dpf(None).await.unwrap().len(),
                usize::from(path.is_empty()),
                "{name}: {controller}"
            );
            if !path.is_empty() {
                assert!(
                    mock.dpu_list_selectors.lock().unwrap().is_empty(),
                    "{name}: {controller}"
                );
            }
        }
    }
}

#[tokio::test]
async fn pending_upgrade_discovers_only_drift_against_reconciled_deployments() {
    let stale = dpu(
        Some("bf-bundle-abc"),
        None,
        Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        "old-flavor",
    );
    let desired = deployment(Some("bf-bundle-abc"), None, true);
    let mut old_generation = desired.clone();
    old_generation.metadata.generation = Some(2);
    let unknown_owner = set_dpu_owner(stale.clone(), "unknown-deployment");
    let mut missing_owner = stale.clone();
    missing_owner.metadata.labels = None;
    let current = dpu(
        Some("bf-bundle-abc"),
        None,
        Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        FLAVOR,
    );

    for (name, dpu, deployment, expected_count) in vec![
        (
            "stale flavor during upgrade",
            stale.clone(),
            desired.clone(),
            1,
        ),
        (
            "unreconciled deployment",
            stale.clone(),
            deployment(Some("bf-bundle-abc"), None, false),
            0,
        ),
        (
            "stale observed generation",
            stale.clone(),
            old_generation,
            0,
        ),
        ("current DPU", current, desired.clone(), 0),
        ("unknown owner", unknown_owner, desired.clone(), 0),
        ("missing owner label", missing_owner, desired, 0),
        (
            "no provisioning source",
            stale.clone(),
            deployment(None, None, true),
            0,
        ),
        (
            "ambiguous provisioning source",
            stale,
            deployment(Some("bf-bundle-abc"), Some("software"), true),
            0,
        ),
    ]
    .into_boxed_slice()
    {
        let mock = upgrade_pending_mock(dpu, deployment);
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
            .build_without_resources()
            .await
            .expect("sdk");
        let mismatches = sdk
            .find_outdated_dpus_dpf(Some("test/controlled=true"))
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(mismatches.len(), expected_count, "{name}");
        if let Some(mismatch) = mismatches.first() {
            assert_eq!(mismatch.dpu_cr_name, DPU_NAME);
            assert_eq!(mismatch.target_source, "test-namespace-bf-bundle-abc.bfb");
            assert_eq!(
                *mock.dpu_list_selectors.lock().unwrap(),
                vec![Some("test/controlled=true".to_string())]
            );
        }
    }
}

#[tokio::test]
async fn replacement_is_current_only_after_its_desired_bfb_is_installed() {
    let mock = upgrade_pending_mock(
        dpu(
            Some("bf-bundle-abc"),
            None,
            Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
            "old-flavor",
        ),
        deployment(Some("bf-bundle-abc"), None, true),
    );
    let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NS, String::new())
        .build_without_resources()
        .await
        .expect("sdk");
    assert_eq!(sdk.find_outdated_dpus_dpf(None).await.unwrap().len(), 1);

    sdk.reprovision_dpu("001", "node-host-001").await.unwrap();
    assert!(mock.dpus.get(DPU_NAME).is_none());

    let mut replacement = dpu(Some("bf-bundle-abc"), None, None, FLAVOR);
    replacement.status.as_mut().unwrap().phase = DpuStatusPhase::Pending;
    mock.dpus.insert(DPU_NAME.to_string(), replacement.clone());
    assert!(sdk.is_dpu_outdated(DPU_NAME).await.unwrap());

    replacement.status.as_mut().unwrap().phase = DpuStatusPhase::Ready;
    mock.dpus.insert(DPU_NAME.to_string(), replacement.clone());
    assert!(sdk.is_dpu_outdated(DPU_NAME).await.unwrap());

    replacement.status.as_mut().unwrap().bfb_file =
        Some("/bfb/test-namespace-bf-bundle-abc.bfb".to_string());
    mock.dpus.insert(DPU_NAME.to_string(), replacement);
    assert!(!sdk.is_dpu_outdated(DPU_NAME).await.unwrap());
    assert!(sdk.find_outdated_dpus_dpf(None).await.unwrap().is_empty());
}
