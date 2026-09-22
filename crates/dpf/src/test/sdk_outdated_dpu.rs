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
