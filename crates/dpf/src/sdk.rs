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

//! DPF SDK - High-level interface for DPF operations.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use carbide_utils::none_if_empty::NoneIfEmpty;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::core::ObjectMeta;
use model::dpa_interface::DpaInterface;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::crds::bfbs_generated::{BFB, BfbSpec};
use crate::crds::bluefieldsoftwares_generated::{BlueFieldSoftware, BlueFieldSoftwareSpec};
use crate::crds::dpudeployments_generated::{
    DPUDeployment, DpuDeploymentDpus, DpuDeploymentDpusDpuSetStrategy,
    DpuDeploymentDpusDpuSetStrategyType, DpuDeploymentDpusDpuSets,
    DpuDeploymentDpusDpuSetsDpuNodeSelector, DpuDeploymentDpusNodeEffect,
    DpuDeploymentServiceChains, DpuDeploymentServiceChainsSwitches,
    DpuDeploymentServiceChainsSwitchesPorts, DpuDeploymentServiceChainsSwitchesPortsService,
    DpuDeploymentServiceChainsSwitchesPortsServiceInterface,
    DpuDeploymentServiceChainsUpgradePolicy, DpuDeploymentServices, DpuDeploymentServicesDependsOn,
    DpuDeploymentSpec,
};
#[cfg(test)]
use crate::crds::dpudevices_generated::DpuDeviceCluster;
use crate::crds::dpudevices_generated::{DPUDevice, DpuDeviceSpec};
use crate::crds::dpunodes_generated::{
    DPUNode, DpuNodeDpus, DpuNodeNodeRebootMethod, DpuNodeNodeRebootMethodExternal, DpuNodeSpec,
};
use crate::crds::dpus_generated::DPU;
use crate::crds::dpuserviceconfigurations_generated::{
    DPUServiceConfiguration, DpuServiceConfigurationInterfaces,
    DpuServiceConfigurationServiceConfiguration,
    DpuServiceConfigurationServiceConfigurationConfigPorts,
    DpuServiceConfigurationServiceConfigurationConfigPortsPorts,
    DpuServiceConfigurationServiceConfigurationConfigPortsPortsProtocol,
    DpuServiceConfigurationServiceConfigurationConfigPortsServiceType,
    DpuServiceConfigurationServiceConfigurationHelmChart,
    DpuServiceConfigurationServiceConfigurationServiceDaemonSet,
    DpuServiceConfigurationServiceConfigurationServiceDaemonSetUpdateStrategy,
    DpuServiceConfigurationServiceConfigurationServiceDaemonSetUpdateStrategyRollingUpdate,
    DpuServiceConfigurationSpec, DpuServiceConfigurationUpgradePolicy,
};
use crate::crds::dpuserviceinterfaces_generated::{
    DPUServiceInterface, DpuServiceInterfaceSpec, DpuServiceInterfaceTemplate,
    DpuServiceInterfaceTemplateSpec, DpuServiceInterfaceTemplateSpecNodeSelector,
    DpuServiceInterfaceTemplateSpecTemplate, DpuServiceInterfaceTemplateSpecTemplateMetadata,
    DpuServiceInterfaceTemplateSpecTemplateSpec,
    DpuServiceInterfaceTemplateSpecTemplateSpecInterfaceType,
    DpuServiceInterfaceTemplateSpecTemplateSpecPatch,
    DpuServiceInterfaceTemplateSpecTemplateSpecPf,
    DpuServiceInterfaceTemplateSpecTemplateSpecPfNicSelector,
    DpuServiceInterfaceTemplateSpecTemplateSpecPfNicSelectorType,
    DpuServiceInterfaceTemplateSpecTemplateSpecPhysical,
    DpuServiceInterfaceTemplateSpecTemplateSpecVf,
    DpuServiceInterfaceTemplateSpecTemplateSpecVfNicSelector,
    DpuServiceInterfaceTemplateSpecTemplateSpecVfNicSelectorType,
};
use crate::crds::dpuservicenads_generated::{
    DPUServiceNAD, DpuServiceNadResourceType, DpuServiceNadSpec,
};
use crate::crds::dpuservices_generated::{
    DPUService, DpuServiceHelmChart, DpuServiceHelmChartSource, DpuServiceSecurity,
    DpuServiceServiceDaemonSet, DpuServiceServiceDaemonSetNodeSelector,
    DpuServiceServiceDaemonSetNodeSelectorNodeSelectorTerms,
    DpuServiceServiceDaemonSetNodeSelectorNodeSelectorTermsMatchExpressions, DpuServiceSpec,
};
use crate::crds::dpuservicetemplates_generated::{
    DPUServiceTemplate, DpuServiceTemplateHelmChart, DpuServiceTemplateHelmChartSource,
    DpuServiceTemplateSpec,
};
use crate::error::DpfError;
use crate::repository::{
    BfbRepository, BlueFieldSoftwareRepository, DpfOperatorConfigRepository,
    DpuDeploymentRepository, DpuDeviceRepository, DpuFlavorRepository, DpuFlavorTemplateRepository,
    DpuNodeMaintenanceRepository, DpuNodeRepository, DpuRepository,
    DpuServiceConfigurationRepository, DpuServiceNADRepository, DpuServiceRepository,
    DpuServiceTemplateRepository, K8sConfigRepository,
};
#[cfg(test)]
use crate::types::DEFAULT_PF_TOTAL_SF_RESERVED;
use crate::types::{
    BlueFieldSoftwareParams, BmcPasswordProvider, ConfigPortsServiceType, DHCP_SERVER_SERVICE_NAME,
    DOCA_HBN_SERVICE_NAME, DOCA_WEAVE_DHCP_AGENT_PF_TOTAL_SF, DPU_AGENT_SERVICE_NAME,
    DPU_ENABLED_NODE_LABEL, DTS_SERVICE_NAME, DetachedDpuServiceDefinition, DpfInterceptBridging,
    DpuDeploymentType, DpuDeviceInfo, DpuDeviceSummary, DpuMismatch, DpuNodeInfo, DpuNodeSummary,
    DpuPhase, DpuServiceHelmChartObservation, DpuServiceInterfacePatch,
    DpuServiceInterfaceTemplateDefinition, DpuServiceInterfaceTemplateType, DpuServiceObservation,
    DpuServiceVersion, DpuSummary, FMDS_SERVICE_NAME, HostDpfSnapshot, InitDpfResourcesConfig,
    MAX_BLUEFIELD_VFS_PER_PF, OTEL_COLLECTOR_SERVICE_NAME, PF_TOTAL_SF_BF4_ASTRA_FUDGE,
    ServiceConfigPortProtocol, ServiceDefinition, ServiceNADResourceType, ServiceTemplateVersion,
};
use crate::watcher::DpuWatcherBuilder;

const SECRET_NAME: &str = "bmc-shared-password";
const BFB_NAME_PREFIX: &str = "bf-bundle";
const BLUEFIELD_SOFTWARE_NAME_PREFIX: &str = "bf-software";
const MAX_HBN_SERVICE_INTERFACES: usize = 32;
/// Label set by DPF on deployment-owned resources and propagated to the corresponding
/// DPU-cluster Node. Value format: `<namespace>_<deployment_name>`.
const DPU_OWNED_BY_DEPLOYMENT_LABEL: &str = "svc.dpu.nvidia.com/owned-by-dpudeployment";

/// Returns DPF's canonical ownership-label value for one DPUDeployment.
fn dpu_deployment_owner_label_value(namespace: &str, deployment_name: &str) -> String {
    format!("{namespace}_{deployment_name}")
}

/// Selects the DPU-cluster Node owned by one DPUDeployment.
fn dpu_cluster_node_selector(namespace: &str, deployment_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(
        DPU_OWNED_BY_DEPLOYMENT_LABEL.to_string(),
        dpu_deployment_owner_label_value(namespace, deployment_name),
    )])
}

pub(crate) const RESTART_ANNOTATION: &str =
    "provisioning.dpu.nvidia.com/dpunode-external-reboot-required";
pub(crate) const HOLD_ANNOTATION: &str = "provisioning.dpu.nvidia.com/wait-for-external-nodeeffect";
/// Provides custom labels for DPF resources.
///
/// Implement this trait to attach caller-specific labels to DPUDevice
/// and DPUNode resources.
pub trait ResourceLabeler: Send + Sync {
    /// Labels to apply to DPUDevice resources on creation.
    fn device_labels(&self, _info: &DpuDeviceInfo) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Static labels applied to DPUNode resources on creation.
    /// Also used for removal patches on node deletion.
    fn node_labels(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Node selector labels for a specific deployment type.
    /// Used by [`build_deployment`] to populate `dpuNodeSelector.matchLabels`.
    /// Returns `ConfigError` if no deployment is configured for the requested type.
    fn node_labels_for_deployment_type(
        &self,
        deployment_type: DpuDeploymentType,
    ) -> Result<BTreeMap<String, String>, crate::DpfError>;

    /// Contextual labels applied to DPUNode resources on creation only.
    /// Unlike `node_labels`, these are NOT used for selectors or removal
    /// patches — they carry per-registration metadata (e.g. machine IDs).
    fn node_context_labels(&self, _info: &DpuNodeInfo) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Optional Kubernetes label selector to scope DPU watches and listings
    /// (e.g. `"app=foo,env=prod"`). Returns `None` by default.
    fn dpu_label_selector(&self) -> Option<String> {
        None
    }
}

/// Default labeler that applies no labels.
pub struct NoLabels;

impl ResourceLabeler for NoLabels {
    fn node_labels_for_deployment_type(
        &self,
        _deployment_type: DpuDeploymentType,
    ) -> Result<BTreeMap<String, String>, crate::DpfError> {
        Ok(BTreeMap::new())
    }
}

/// The main DPF SDK interface.
///
/// This SDK provides high-level operations for managing DPF resources,
/// abstracting away the details of Kubernetes CRD manipulation.
///
/// Trait bounds are on the impl blocks, not the struct, so tests can
/// instantiate `DpfSdk` with a mock that only implements the traits
/// needed by the methods under test.
///
/// Construct via [`DpfSdkBuilder`].
pub struct DpfSdk<R, L = NoLabels> {
    repo: Arc<R>,
    namespace: String,
    labeler: L,
    _bmc_refresh_guard: Option<tokio_util::sync::DropGuard>,
}

impl<R, L> DpfSdk<R, L> {
    /// Get the namespace this SDK operates in.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Get a reference to the repository.
    pub fn repo(&self) -> &Arc<R> {
        &self.repo
    }
}

/// Builder for [`DpfSdk`].
pub struct DpfSdkBuilder<'a, R, P, L = NoLabels> {
    repo: R,
    namespace: String,
    labeler: L,
    bmc_password_provider: P,
    bmc_password_refresh_interval: Option<Duration>,
    join_set: Option<&'a mut tokio::task::JoinSet<()>>,
}

impl<R, P> DpfSdkBuilder<'_, R, P> {
    pub fn new(repo: R, namespace: impl Into<String>, bmc_password_provider: P) -> Self {
        DpfSdkBuilder {
            repo,
            namespace: namespace.into(),
            labeler: NoLabels,
            bmc_password_provider,
            bmc_password_refresh_interval: None,
            join_set: None,
        }
    }
}

impl<'a, R, P, L> DpfSdkBuilder<'a, R, P, L> {
    // enables custom labels to be applied to the DPUDevice and DPUNode resources.
    pub fn with_labeler<L2>(self, labeler: L2) -> DpfSdkBuilder<'a, R, P, L2> {
        DpfSdkBuilder {
            repo: self.repo,
            namespace: self.namespace,
            labeler,
            bmc_password_provider: self.bmc_password_provider,
            bmc_password_refresh_interval: self.bmc_password_refresh_interval,
            join_set: self.join_set,
        }
    }

    // enables background refresh of the BMC password.
    pub fn with_bmc_password_refresh_interval(mut self, interval: Duration) -> Self {
        self.bmc_password_refresh_interval = Some(interval);
        self
    }

    /// Spawn background tasks into the provided `JoinSet` instead of
    /// via `tokio::spawn`. Use this in production to join all background
    /// tasks via a single `JoinSet` to catch panics.
    pub fn with_join_set(mut self, join_set: &'a mut tokio::task::JoinSet<()>) -> Self {
        self.join_set = Some(join_set);
        self
    }
}

impl<R, P, L> DpfSdkBuilder<'_, R, P, L>
where
    R: K8sConfigRepository + 'static,
    P: BmcPasswordProvider + 'static,
{
    /// Fetch password, write the K8s BMC secret, spawn refresh task,
    /// and return the constructed SDK.
    ///
    /// The BMC password is not necessarily available the first time this runs.
    /// It comes from the site-wide BMC root credential, which operators set
    /// *through the API this SDK is initializing*, so on a fresh site the
    /// credential does not exist yet. When a refresh interval is configured the
    /// initial read is therefore best-effort: initialization continues without
    /// the Secret, and the refresh task writes it as soon as the credential
    /// appears — no restart needed. Without a refresh interval nothing would
    /// ever retry, so there a failed read stays fatal.
    async fn init_secret_and_task(self) -> Result<DpfSdk<R, L>, DpfError> {
        let repo = Arc::new(self.repo);
        let namespace = self.namespace;
        let provider = self.bmc_password_provider;

        let password = match provider.get_bmc_password().await {
            Ok(password) => {
                write_bmc_secret::<R>(&repo, &namespace, &password).await?;
                Some(password)
            }
            Err(error) if self.bmc_password_refresh_interval.is_some() => {
                tracing::warn!(
                    %error,
                    secret = SECRET_NAME,
                    "BMC password unavailable; DPF secret will be written once the credential is set"
                );
                None
            }
            Err(error) => return Err(error),
        };

        let guard = if let Some(interval) = self.bmc_password_refresh_interval {
            Some(spawn_bmc_refresh(
                repo.clone(),
                namespace.clone(),
                provider,
                password,
                interval,
                self.join_set,
            )?)
        } else {
            None
        };

        Ok(DpfSdk {
            repo,
            namespace,
            labeler: self.labeler,
            _bmc_refresh_guard: guard,
        })
    }

    /// Consume the builder, create the K8s BMC secret and optionally
    /// spawn a background refresh task. Does not create DPF CRDs.
    pub async fn build_without_resources(self) -> Result<DpfSdk<R, L>, DpfError> {
        self.init_secret_and_task().await
    }
}

impl<R, P, L> DpfSdkBuilder<'_, R, P, L>
where
    R: BfbRepository
        + BlueFieldSoftwareRepository
        + DpuFlavorRepository
        + DpuFlavorTemplateRepository
        + DpuDeploymentRepository
        + DpuServiceTemplateRepository
        + DpuServiceConfigurationRepository
        + DpuServiceNADRepository
        + crate::repository::DpuServiceInterfaceRepository
        + K8sConfigRepository
        + DpfOperatorConfigRepository
        + 'static,
    P: BmcPasswordProvider + 'static,
    L: ResourceLabeler,
{
    /// Consume the builder, create the K8s BMC secret, create all
    /// initialization CRDs, and optionally spawn a background refresh task.
    pub async fn initialize(
        self,
        config: &InitDpfResourcesConfig,
    ) -> Result<DpfSdk<R, L>, DpfError> {
        // Validate before `init_secret_and_task` writes the shared BMC Secret.
        let resolved = resolve_initialization_inventory(config)?;
        let sdk = self.init_secret_and_task().await?;
        sdk.create_initialization_objects_resolved(config, resolved)
            .await?;
        Ok(sdk)
    }
}

async fn write_bmc_secret<R: K8sConfigRepository>(
    repo: &R,
    namespace: &str,
    password: &str,
) -> Result<(), DpfError> {
    let mut data = BTreeMap::new();
    data.insert("password".to_string(), password.as_bytes().to_vec());
    K8sConfigRepository::apply_secret(repo, SECRET_NAME, namespace, data).await
}

/// Fetch the current BMC password from the provider and update the K8s
/// secret when it differs from `last_password`. Returns the password
/// value that should be remembered for the next comparison.
///
/// `last_password` is `None` when no password has been written yet — either
/// because the credential was unset at startup, or because every write since
/// has failed. That case writes on the next successful read, which is how a
/// site that boots without the site-wide BMC root recovers on its own.
async fn refresh_bmc_secret_if_changed<R: K8sConfigRepository>(
    repo: &R,
    namespace: &str,
    provider: &impl BmcPasswordProvider,
    last_password: Option<String>,
) -> Option<String> {
    match provider.get_bmc_password().await {
        Ok(new_pw) if Some(&new_pw) != last_password.as_ref() => {
            if let Err(e) = write_bmc_secret::<R>(repo, namespace, &new_pw).await {
                tracing::error!(error = %e, "Failed to refresh BMC secret");
                last_password
            } else {
                Some(new_pw)
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to read BMC password");
            last_password
        }
        _ => last_password,
    }
}

// separate function to drop the 'a lifetime from the builder
fn spawn_bmc_refresh<R, P>(
    repo: Arc<R>,
    namespace: String,
    provider: P,
    password: Option<String>,
    interval: Duration,
    join_set: Option<&mut tokio::task::JoinSet<()>>,
) -> Result<tokio_util::sync::DropGuard, DpfError>
where
    R: K8sConfigRepository + 'static,
    P: BmcPasswordProvider + 'static,
{
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let guard = cancel_token.clone().drop_guard();
    let task = async move {
        let mut last_password = password;
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        while cancel_token
            .run_until_cancelled(ticker.tick())
            .await
            .is_some()
        {
            last_password =
                refresh_bmc_secret_if_changed(repo.as_ref(), &namespace, &provider, last_password)
                    .await;
        }
    };

    if let Some(js) = join_set {
        js.build_task()
            .name("dpf_bmc_password_refresh")
            .spawn(task)
            .map_err(|e| {
                DpfError::InvalidState(format!("Failed to spawn BMC refresh task: {e}"))
            })?;
    } else {
        tokio::task::Builder::new()
            .name("dpf_bmc_password_refresh")
            .spawn(task)
            .map_err(|e| {
                DpfError::InvalidState(format!("Failed to spawn BMC refresh task: {e}"))
            })?;
    }

    Ok(guard)
}

/// DPUNode CR name: `node-{node_id}`.
/// `node_id` is a compact, stable machine identifier (e.g. `01-02-03-04-05-06`).
/// The DPF CRD limits resource names to 48 characters.
pub fn dpu_node_cr_name(node_id: &str) -> String {
    format!("node-{}", node_id)
}

/// DPUDevice CR name: `device-{device_id}`.
/// The DPF operator uses the DPUDevice CR name verbatim when constructing
/// the DPU CR name (`{dpuNodeName}-{dpuDeviceName}`), so the `device-`
/// prefix produces the expected `node-{node_id}-device-{device_id}` format.
pub fn dpu_device_cr_name(device_id: &str) -> String {
    format!("device-{}", device_id)
}

/// DPU CR name: `node-{node_id}-device-{device_id}`.
/// This matches the DPF operator's naming: `{dpuNodeName}-{dpuDeviceName}`
/// where dpuNodeName = `node-{node_id}` and dpuDeviceName = `device-{device_id}`.
pub fn dpu_cr_name(device_id: &str, node_id: &str) -> String {
    format!(
        "{}-{}",
        dpu_node_cr_name(node_id),
        dpu_device_cr_name(device_id)
    )
}

/// Extract the node ID from a DPUNode CR name by stripping the `node-` prefix.
pub fn node_id_from_dpu_node_cr_name(node_cr_name: &str) -> &str {
    node_cr_name.strip_prefix("node-").unwrap_or(node_cr_name)
}

impl<R, L: ResourceLabeler> DpfSdk<R, L> {
    /// Build a JSON patch that nulls every node label key.
    fn node_label_removal_patch(&self) -> serde_json::Value {
        let nulls: serde_json::Map<String, serde_json::Value> = self
            .labeler
            .node_labels()
            .keys()
            .map(|k| (k.clone(), serde_json::Value::Null))
            .collect();
        json!({ "metadata": { "labels": nulls } })
    }
}

/// Must match the `configMapKeyRef.key` the BF4 flavors declare in `flavor.rs`.
const EXTRA_SCRIPT_CONFIGMAP_KEY: &str = "script";

/// No-op body seeded into a new extra-script ConfigMap; the flavor runs it by path.
const EXTRA_SCRIPT_PLACEHOLDER: &str =
    "#!/usr/bin/env bash\necho \"NICo extra script: nothing to run\"\n";

/// Must match the Astra flavor's `configMapKeyRef` in `flavor.rs`.
const BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_NAME: &str = "ra2.2-runtime";
const BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_KEY: &str = "RA2.2-runtime.yaml";

/// ConfigMaps the deployment's flavor references, in run order. Empty for BF3,
/// which inlines its scripts instead of using `contentFrom`.
fn extra_script_configmap_names(deployment_type: DpuDeploymentType) -> &'static [&'static str] {
    match deployment_type {
        DpuDeploymentType::Bf3 | DpuDeploymentType::Bf3Gb200 => &[],
        DpuDeploymentType::Bf4Generic => &[
            "extra-script-pre-ovs-bf4-generic",
            "extra-script-post-ovs-bf4-generic",
        ],
        DpuDeploymentType::Bf4Astra => &[
            "extra-script-pre-ovs-bf4-astra",
            "extra-script-post-ovs-bf4-astra",
        ],
    }
}

/// Seed the extra-script ConfigMaps a BF4 DPUFlavor sources via `contentFrom`.
///
/// Create-only. NICo never updates these: the content belongs to the operator, so
/// an existing ConfigMap is left exactly as found. A plain create is also atomic,
/// so an edit racing this cannot be clobbered.
async fn create_extra_script_configmaps<R: K8sConfigRepository>(
    repo: &R,
    namespace: &str,
    deployment_type: DpuDeploymentType,
) -> Result<(), DpfError> {
    for name in extra_script_configmap_names(deployment_type) {
        let data = BTreeMap::from([(
            EXTRA_SCRIPT_CONFIGMAP_KEY.to_string(),
            EXTRA_SCRIPT_PLACEHOLDER.to_string(),
        )]);
        if repo.create_configmap(name, namespace, data).await? {
            tracing::info!(
                configmap = %name,
                %namespace,
                "Created extra-script ConfigMap with a no-op placeholder script"
            );
        } else {
            tracing::debug!(
                configmap = %name,
                %namespace,
                "Extra-script ConfigMap already exists; leaving it untouched"
            );
        }
    }
    Ok(())
}

/// Require the RA2.2 runtime ConfigMap referenced by the BF4 Astra flavor.
async fn validate_bf4_astra_ra2_2_runtime_configmap<R: K8sConfigRepository>(
    repo: &R,
    namespace: &str,
    deployment_type: DpuDeploymentType,
) -> Result<(), DpfError> {
    if deployment_type != DpuDeploymentType::Bf4Astra {
        return Ok(());
    }

    let configmap = repo
        .get_configmap(BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_NAME, namespace)
        .await?
        .ok_or_else(|| {
            DpfError::ConfigError(format!(
                "BF4 Astra requires Spectrum-X runtime ConfigMap {namespace}/{} with key {}; create it before initializing Astra",
                BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_NAME,
                BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_KEY,
            ))
        })?;
    if !configmap.contains_key(BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_KEY) {
        return Err(DpfError::ConfigError(format!(
            "BF4 Astra Spectrum-X runtime ConfigMap {namespace}/{} must contain key {}",
            BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_NAME, BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_KEY,
        )));
    }
    Ok(())
}

async fn create_bfb<R: BfbRepository>(
    repo: &R,
    namespace: &str,
    bfb_url: &str,
) -> Result<String, DpfError> {
    let bfb_name = format!(
        "{}-{}",
        BFB_NAME_PREFIX,
        hex::encode(Sha256::digest(bfb_url.as_bytes()))
    );

    let bfb = BFB {
        metadata: ObjectMeta {
            name: Some(bfb_name.clone()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: BfbSpec {
            url: bfb_url.to_string(),
            file_name: None,
            versions: None,
        },
        status: None,
    };
    match BfbRepository::create(repo, &bfb).await {
        Ok(_) => Ok(bfb_name),
        Err(DpfError::KubeError(kube::Error::Api(ref err)))
            if err.is_already_exists() || err.is_conflict() =>
        {
            tracing::debug!(bfb = %bfb_name, "BFB already exists, reusing");
            Ok(bfb_name)
        }
        Err(e) => Err(e),
    }
}

/// Reference to the resource a DPUDeployment provisions DPUs from. Exactly one
/// variant is populated per deployment, matching the DPUDeployment CRD rule that
/// exactly one of `spec.dpus.bfb` / `spec.dpus.blueFieldSoftware` be set.
pub enum DpuProvisioningSource {
    /// Name of a `BFB` CR (BF3-class DPUs).
    Bfb(String),
    /// Name of a `BlueFieldSoftware` CR (BF4-class DPUs).
    BlueFieldSoftware(String),
}

/// Creates a `BlueFieldSoftware` CR with a hash-derived name
/// (`{prefix}-{sha256(os_iso[+pldm_fw_bundle])}`). Like [`create_bfb`], any
/// change to the spec produces a new name so DPUs are detected as outdated.
/// Idempotent: an already-existing CR with the same name is reused.
async fn create_bluefield_software<R: BlueFieldSoftwareRepository>(
    repo: &R,
    namespace: &str,
    params: &BlueFieldSoftwareParams,
) -> Result<String, DpfError> {
    let mut hasher = Sha256::new();
    hasher.update(params.os_iso.as_bytes());
    if let Some(pldm) = params.pldm_fw_bundle.as_deref() {
        hasher.update(b"\0");
        hasher.update(pldm.as_bytes());
    }
    let name = format!(
        "{}-{}",
        BLUEFIELD_SOFTWARE_NAME_PREFIX,
        hex::encode(hasher.finalize())
    );

    let bfs = BlueFieldSoftware {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: BlueFieldSoftwareSpec {
            os_iso: params.os_iso.clone(),
            pldm_fw_bundle: params.pldm_fw_bundle.clone(),
            nic_fw: None,
            platform_pldm_fw_bundle: None,
            force_fw_update: None,
        },
        status: None,
    };
    match BlueFieldSoftwareRepository::create(repo, &bfs).await {
        Ok(_) => Ok(name),
        Err(DpfError::KubeError(kube::Error::Api(ref err)))
            if err.is_already_exists() || err.is_conflict() =>
        {
            // Reuse the existing CR only if it is not being torn down; otherwise
            // the DPUDeployment would reference a source that is disappearing.
            let existing = BlueFieldSoftwareRepository::get(repo, &name, namespace).await?;
            if existing
                .as_ref()
                .is_some_and(|b| b.metadata.deletion_timestamp.is_some())
            {
                return Err(DpfError::InvalidState(format!(
                    "BlueFieldSoftware {name} is being deleted (has deletionTimestamp); \
                     cannot reuse until the old resource is fully removed"
                )));
            }
            tracing::debug!(bluefield_software = %name, "BlueFieldSoftware already exists, reusing");
            Ok(name)
        }
        Err(e) => Err(e),
    }
}

/// Creates a DPUFlavor with a hash-derived name (`{default_flavor_name}-{spec_hash}`).
/// Any change in the spec produces a different hash and therefore a new flavor name, which
/// causes MachineUpdateManager to detect the DPUs as outdated and trigger reprovisioning.
async fn create_dpu_flavor<R: DpuFlavorRepository>(
    repo: &R,
    namespace: &str,
    config: &InitDpfResourcesConfig,
    resolved: &ResolvedInitialization<'_>,
) -> Result<String, DpfError> {
    let mut flavor = crate::flavor::default_flavor_for_with_topology(
        namespace,
        &config.proxy,
        config.deployment_type,
        config.num_of_vfs,
        resolved.pf_total_sf,
        config.intercept_bridging.as_ref(),
        config
            .intercept_bridging
            .as_ref()
            .map(|_| resolved.interfaces.as_ref()),
        &config.extra_bfcfg_parameters,
    )?;
    let name = flavor.unique_name(&config.flavor_name)?;
    flavor.metadata.name = Some(name.clone());

    match DpuFlavorRepository::create(repo, &flavor).await {
        Ok(_) => Ok(name),
        Err(DpfError::KubeError(kube::Error::Api(ref err)))
            if err.is_already_exists() || err.is_conflict() =>
        {
            // The hash-named flavor already exists (e.g. created by a concurrent reconcile).
            // Guard against the case where it is being deleted — re-creating while
            // deletionTimestamp is set would conflict again once the finalizers clear.
            let existing = DpuFlavorRepository::get(repo, &name, namespace).await?;
            match existing {
                None => Err(DpfError::InvalidState(format!(
                    "DPUFlavor {name} disappeared after AlreadyExists conflict; \
                     will retry on next reconcile",
                ))),
                Some(f) if f.metadata.deletion_timestamp.is_some() => {
                    Err(DpfError::InvalidState(format!(
                        "DPUFlavor {name} is being deleted (has deletionTimestamp); \
                         cannot re-create until the old resource is fully removed",
                    )))
                }
                Some(_) => {
                    tracing::debug!(flavor = %name, "DPU flavor already exists");
                    Ok(name)
                }
            }
        }
        Err(e) => Err(e),
    }
}

/// Creates the Astra flavor template with a hash-derived name.
async fn create_dpu_flavor_template<R: DpuFlavorTemplateRepository>(
    repo: &R,
    namespace: &str,
    config: &InitDpfResourcesConfig,
    resolved: &ResolvedInitialization<'_>,
) -> Result<String, DpfError> {
    let mut template = crate::flavor::flavor_bf4_astra(
        namespace,
        &config.proxy,
        resolved.pf_total_sf,
        &config.extra_bfcfg_parameters,
    )?;
    let name = template.unique_name(&config.flavor_name)?;
    template.metadata.name = Some(name.clone());

    match DpuFlavorTemplateRepository::create(repo, &template).await {
        Ok(_) => {
            tracing::info!(flavor_template = %name, "DPU flavor template created");
            Ok(name)
        }
        Err(DpfError::KubeError(kube::Error::Api(ref err)))
            if err.is_already_exists() || err.is_conflict() =>
        {
            let existing = DpuFlavorTemplateRepository::get(repo, &name, namespace).await?;
            match existing {
                None => Err(DpfError::InvalidState(format!(
                    "DPUFlavorTemplate {name} disappeared after AlreadyExists conflict; \
                     will retry on next reconcile",
                ))),
                Some(template) if template.metadata.deletion_timestamp.is_some() => {
                    Err(DpfError::InvalidState(format!(
                        "DPUFlavorTemplate {name} is being deleted (has deletionTimestamp); \
                         cannot re-create until the old resource is fully removed",
                    )))
                }
                Some(_) => {
                    tracing::info!(flavor_template = %name, "DPU flavor template already exists");
                    Ok(name)
                }
            }
        }
        Err(e) => Err(e),
    }
}

/// Short, per-deployment suffix appended to service CR names so that each
/// DPUDeployment gets its own DPUServiceTemplate/Configuration/NAD CRs. Without
/// this, two deployments in the same namespace (e.g. BF3 and BF4) would create
/// identically-named CRs and the second `apply` would overwrite the first's
/// Helm values/version.
///
/// BF3 intentionally uses an empty suffix so its CR names are unchanged — this
/// keeps existing BF3 clusters untouched (no CR rename / orphaning on upgrade).
/// Additional deployment classes are suffixed to avoid colliding with BF3.
pub fn deployment_cr_suffix(deployment_type: DpuDeploymentType) -> &'static str {
    match deployment_type {
        DpuDeploymentType::Bf3 => "",
        DpuDeploymentType::Bf3Gb200 => "bf3gb200",
        DpuDeploymentType::Bf4Generic => "bf4generic",
        DpuDeploymentType::Bf4Astra => "bf4astra",
    }
}

/// Suffix appended to deployment-scoped DPUServiceInterface CR names.
///
/// Unlike the existing service CR compatibility scheme, every deployment type
/// is suffixed. The migration is opt-in; operators must remove the old
/// generation manually because NICo neither detects nor deletes it.
fn service_interface_cr_suffix(deployment_type: DpuDeploymentType) -> &'static str {
    match deployment_type {
        DpuDeploymentType::Bf3 => "bf3",
        DpuDeploymentType::Bf3Gb200 => "bf3gb200",
        DpuDeploymentType::Bf4Generic => "bf4",
        DpuDeploymentType::Bf4Astra => "astra",
    }
}

/// Per-deployment CR name for a service or NAD: its logical name with the
/// deployment suffix appended (or the logical name unchanged when the suffix is
/// empty, as for BF3). The logical name (used as the DPUDeployment `services`
/// map key, `deploymentServiceName`, `dependsOn`, and service-chain references)
/// is left unchanged; only the CR `metadata.name` and the references pointing at
/// it are suffixed.
fn service_cr_name(logical_name: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        logical_name.to_string()
    } else {
        format!("{logical_name}-{suffix}")
    }
}

pub fn build_service_template(
    svc: &ServiceDefinition,
    namespace: &str,
    suffix: &str,
) -> DPUServiceTemplate {
    let helm_values: Option<BTreeMap<String, serde_json::Value>> =
        svc.helm_values.as_ref().and_then(|v| {
            v.as_object()
                .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        });

    DPUServiceTemplate {
        metadata: ObjectMeta {
            name: Some(service_cr_name(&svc.name, suffix)),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: DpuServiceTemplateSpec {
            deployment_service_name: svc.name.clone(),
            helm_chart: DpuServiceTemplateHelmChart {
                source: DpuServiceTemplateHelmChartSource {
                    chart: Some(svc.helm_chart.clone()),
                    path: None,
                    release_name: None,
                    repo_url: svc.helm_repo_url.clone(),
                    version: svc.helm_version.clone(),
                },
                values: helm_values,
            },
            resource_requirements: None,
            security: None,
        },
        status: None,
    }
}

pub fn build_service_configuration(
    svc: &ServiceDefinition,
    namespace: &str,
    suffix: &str,
    nad_rename: &BTreeMap<String, String>,
) -> DPUServiceConfiguration {
    let interfaces: Vec<DpuServiceConfigurationInterfaces> = svc
        .interfaces
        .iter()
        .map(|i| DpuServiceConfigurationInterfaces {
            name: i.name.clone(),
            // A `network` that names a NAD created for this deployment is
            // suffixed to match the (now per-deployment) NAD CR name. Networks
            // that are not deployment-local NADs are left untouched.
            network: nad_rename
                .get(&i.network)
                .cloned()
                .unwrap_or_else(|| i.network.clone()),
            virtual_network: None,
        })
        .collect();

    let config_ports_crd = svc.config_ports.as_ref().and_then(|ports| {
        svc.config_ports_service_type.map(|st| {
            DpuServiceConfigurationServiceConfigurationConfigPorts {
                ports: ports
                    .iter()
                    .map(|p| DpuServiceConfigurationServiceConfigurationConfigPortsPorts {
                        name: p.name.clone(),
                        node_port: p.node_port,
                        port: p.port,
                        protocol: match p.protocol {
                            ServiceConfigPortProtocol::Tcp => {
                                DpuServiceConfigurationServiceConfigurationConfigPortsPortsProtocol::Tcp
                            }
                            ServiceConfigPortProtocol::Udp => {
                                DpuServiceConfigurationServiceConfigurationConfigPortsPortsProtocol::Udp
                            }
                        },
                    })
                    .collect(),
                service_type: match st {
                    ConfigPortsServiceType::NodePort => {
                        DpuServiceConfigurationServiceConfigurationConfigPortsServiceType::NodePort
                    }
                    ConfigPortsServiceType::ClusterIp => {
                        DpuServiceConfigurationServiceConfigurationConfigPortsServiceType::ClusterIp
                    }
                    ConfigPortsServiceType::None => {
                        DpuServiceConfigurationServiceConfigurationConfigPortsServiceType::None
                    }
                },
            }
        })
    });

    let helm_chart_config = svc.config_values.as_ref().and_then(|v| {
        v.as_object().map(|obj| {
            let values: BTreeMap<String, serde_json::Value> =
                obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            DpuServiceConfigurationServiceConfigurationHelmChart {
                values: Some(values),
            }
        })
    });

    let service_daemon_set = DpuServiceConfigurationServiceConfigurationServiceDaemonSet {
        annotations: svc.service_daemon_set_annotations.clone(),
        labels: None,
        resources: svc.service_daemon_set_resources.clone(),
        update_strategy: Some(
            DpuServiceConfigurationServiceConfigurationServiceDaemonSetUpdateStrategy {
                rolling_update: Some(
                    DpuServiceConfigurationServiceConfigurationServiceDaemonSetUpdateStrategyRollingUpdate {
                        max_surge: None,
                        max_unavailable: Some(IntOrString::String("100%".to_string())),
                    },
                ),
                r#type: Some("RollingUpdate".into()),
            },
        ),
    };

    let service_configuration = Some(DpuServiceConfigurationServiceConfiguration {
        config_ports: config_ports_crd,
        deploy_in_cluster: None,
        helm_chart: helm_chart_config,
        service_daemon_set: Some(service_daemon_set),
    });

    DPUServiceConfiguration {
        metadata: ObjectMeta {
            name: Some(service_cr_name(&svc.name, suffix)),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: DpuServiceConfigurationSpec {
            deployment_service_name: svc.name.clone(),
            interfaces: interfaces.none_if_empty(),
            service_configuration,
            // Treat a service update as disruptive so DPF creates a new revision
            // and parks the DPU in the NodeEffect phase instead of restarting
            // services underneath whatever is running. That phase is the gate
            // carbide opens once it has confirmed the DPU is not still awaiting
            // reprovisioning -- see the DPU service sync handler.
            upgrade_policy: DpuServiceConfigurationUpgradePolicy {
                apply_node_effect: Some(true),
            },
        },
    }
}

pub fn build_service_nad(
    svc: &ServiceDefinition,
    namespace: &str,
    suffix: &str,
) -> Option<DPUServiceNAD> {
    svc.service_nad.as_ref().map(|service_nad| DPUServiceNAD {
        metadata: ObjectMeta {
            name: Some(service_cr_name(&service_nad.name, suffix)),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: DpuServiceNadSpec {
            bridge: service_nad.bridge.clone(),
            chained_cn_is: None,
            ipam: service_nad.ipam,
            dpu_cluster_selector: None,
            resource_type: match service_nad.resource_type {
                ServiceNADResourceType::Sf => DpuServiceNadResourceType::Sf,
                ServiceNADResourceType::Vf => DpuServiceNadResourceType::Vf,
                ServiceNADResourceType::Veth => DpuServiceNadResourceType::Veth,
            },
            service_mtu: service_nad.mtu,
        },
        status: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn build_deployment(
    services: &[ServiceDefinition],
    deployment_name: &str,
    source: &DpuProvisioningSource,
    flavor_name: &str,
    namespace: &str,
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
    deployment_node_labels: BTreeMap<String, String>,
    deployment_type: DpuDeploymentType,
) -> DPUDeployment {
    let suffix = deployment_cr_suffix(deployment_type);
    let services_map: BTreeMap<String, DpuDeploymentServices> = services
        .iter()
        .map(|svc| {
            (
                svc.name.clone(),
                DpuDeploymentServices {
                    depends_on: match svc.name.as_str() {
                        DPU_AGENT_SERVICE_NAME => Some(vec![
                            DpuDeploymentServicesDependsOn {
                                name: DHCP_SERVER_SERVICE_NAME.to_string(),
                            },
                            DpuDeploymentServicesDependsOn {
                                name: FMDS_SERVICE_NAME.to_string(),
                            },
                            DpuDeploymentServicesDependsOn {
                                name: DOCA_HBN_SERVICE_NAME.to_string(),
                            },
                        ]),
                        OTEL_COLLECTOR_SERVICE_NAME => Some(vec![
                            DpuDeploymentServicesDependsOn {
                                name: DPU_AGENT_SERVICE_NAME.to_string(),
                            },
                            DpuDeploymentServicesDependsOn {
                                name: FMDS_SERVICE_NAME.to_string(),
                            },
                            // otelcol templates the DTS scrape target from
                            // `{{ (index .Services "dts").Name }}`; without this
                            // dependency that lookup renders empty.
                            DpuDeploymentServicesDependsOn {
                                name: DTS_SERVICE_NAME.to_string(),
                            },
                        ]),

                        _ => None,
                    },
                    // The map key stays the logical service name (so dependsOn
                    // and service chains resolve), but the template/config
                    // references point at the per-deployment CR names.
                    service_configuration: Some(service_cr_name(&svc.name, suffix)),
                    service_template: Some(service_cr_name(&svc.name, suffix)),
                },
            )
        })
        .collect();

    let mut all_switches = Vec::new();
    for iface in interfaces {
        let Some(chained_svc_if) = iface.chained_svc_if.as_ref() else {
            continue;
        };

        let mut ports = vec![DpuDeploymentServiceChainsSwitchesPorts {
            service_interface: Some(DpuDeploymentServiceChainsSwitchesPortsServiceInterface {
                match_labels: BTreeMap::from([("interface".to_string(), iface.name.clone())]),
                ipam: None,
            }),
            service: None,
        }];

        for (service_name, chain_ifname) in chained_svc_if {
            ports.push(DpuDeploymentServiceChainsSwitchesPorts {
                service_interface: None,
                service: Some(DpuDeploymentServiceChainsSwitchesPortsService {
                    name: service_name.clone(),
                    interface: chain_ifname.clone(),
                    ipam: None,
                }),
            });
        }

        all_switches.push(DpuDeploymentServiceChainsSwitches {
            ports,
            service_mtu: None,
        });
    }
    if matches!(deployment_type, DpuDeploymentType::Bf4Astra) {
        all_switches.extend(build_astra_patch_service_chain_switches(interfaces));
    }

    let service_chains = if all_switches.is_empty() {
        None
    } else {
        Some(DpuDeploymentServiceChains {
            switches: all_switches,
            // Disruptive for the same reason as the per-service policy above: a
            // service-chain change must wait for carbide to release the hold.
            upgrade_policy: DpuDeploymentServiceChainsUpgradePolicy {
                apply_node_effect: Some(true),
            },
        })
    };

    let mut node_labels =
        BTreeMap::from([(DPU_ENABLED_NODE_LABEL.to_string(), "true".to_string())]);
    node_labels.extend(deployment_node_labels);

    DPUDeployment {
        metadata: ObjectMeta {
            name: Some(deployment_name.to_string()),
            namespace: Some(namespace.to_string()),
            annotations: Some(BTreeMap::from([(
                "svc.dpu.nvidia.com/dpudeployment-skip-chain-requestor".to_string(),
                "".to_string(),
            )])),
            ..Default::default()
        },
        spec: DpuDeploymentSpec {
            dpus: DpuDeploymentDpus {
                bfb: match source {
                    DpuProvisioningSource::Bfb(name) => Some(name.clone()),
                    DpuProvisioningSource::BlueFieldSoftware(_) => None,
                },
                dpu_sets: Some(vec![DpuDeploymentDpusDpuSets {
                    dpu_annotations: None,
                    dpu_selector: None,
                    name_suffix: "default".to_string(),
                    dpu_node_selector: Some(DpuDeploymentDpusDpuSetsDpuNodeSelector {
                        match_expressions: None,
                        match_labels: Some(node_labels),
                    }),
                    dpu_cluster_selector: None,
                    dpu_device_selector: None,
                    node_selector: None,
                }]),
                flavor: (!matches!(deployment_type, DpuDeploymentType::Bf4Astra))
                    .then(|| flavor_name.to_string()),
                node_effect: DpuDeploymentDpusNodeEffect {
                    custom_action: None,
                    custom_label: None,
                    drain: None,
                    force: Some(false),
                    hold: Some(true),
                    no_effect: None,
                    taint: None,
                },
                dpu_set_strategy: DpuDeploymentDpusDpuSetStrategy {
                    rolling_update: None,
                    r#type: DpuDeploymentDpusDpuSetStrategyType::OnDelete,
                },
                secure_boot: None,
                astra_enabled: matches!(deployment_type, DpuDeploymentType::Bf4Astra)
                    .then_some(true),
                blue_field_software: match source {
                    DpuProvisioningSource::Bfb(_) => None,
                    DpuProvisioningSource::BlueFieldSoftware(name) => Some(name.clone()),
                },
                flavor_template: matches!(deployment_type, DpuDeploymentType::Bf4Astra)
                    .then(|| flavor_name.to_string()),
            },
            revision_history_limit: None,
            service_chains,
            services: services_map,
        },
        status: None,
    }
}

fn build_astra_patch_service_chain_switches(
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
) -> Vec<DpuDeploymentServiceChainsSwitches> {
    let interface_names = interfaces
        .iter()
        .map(|interface| interface.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    astra_xplane_group_ids()
        .iter()
        .filter_map(|group_id| {
            let cx_interface = format!("p-brcx-{group_id}-to-br-sfc");
            let xplane_interface = format!("p-br-xplane-{group_id}-to-br-sfc");
            (interface_names.contains(cx_interface.as_str())
                && interface_names.contains(xplane_interface.as_str()))
            .then(|| DpuDeploymentServiceChainsSwitches {
                ports: [cx_interface, xplane_interface]
                    .into_iter()
                    .map(|interface| DpuDeploymentServiceChainsSwitchesPorts {
                        service_interface: Some(
                            DpuDeploymentServiceChainsSwitchesPortsServiceInterface {
                                match_labels: BTreeMap::from([(
                                    "interface".to_string(),
                                    interface,
                                )]),
                                ipam: None,
                            },
                        ),
                        service: None,
                    })
                    .collect(),
                service_mtu: None,
            })
        })
        .collect()
}

fn astra_xplane_group_ids() -> [&'static str; 8] {
    [
        "r0swpln0", "r1swpln0", "r0swpln1", "r1swpln1", "r2swpln0", "r3swpln0", "r2swpln1",
        "r3swpln1",
    ]
}

pub fn build_dpu_interfaces_vec() -> Vec<DpuServiceInterfaceTemplateDefinition> {
    let interfaces: Vec<DpuServiceInterfaceTemplateDefinition> = vec![
        DpuServiceInterfaceTemplateDefinition {
            name: "p0".into(),
            iface_type: DpuServiceInterfaceTemplateType::Physical,
            pf_id: 0,
            vf_id: 0,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "p0_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0hpf".into(),
            iface_type: DpuServiceInterfaceTemplateType::Pf,
            pf_id: 0,
            vf_id: 0,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0hpf_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0hpf_if".into()),
                (FMDS_SERVICE_NAME.into(), "f_pf0hpf_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf0".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 0,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf0_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf0_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf1".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 1,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf1_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf1_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf2".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 2,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf2_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf2_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf3".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 3,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf3_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf3_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf4".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 4,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf4_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf4_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf5".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 5,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf5_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf5_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf6".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 6,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf6_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf6_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf7".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 7,
            chained_svc_if: Some(vec![
                (DOCA_HBN_SERVICE_NAME.into(), "pf0vf7_if".into()),
                (DHCP_SERVER_SERVICE_NAME.into(), "d_pf0vf7_if".into()),
            ]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf8".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 8,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "pf0vf8_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf9".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 9,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "pf0vf9_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf10".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 10,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "pf0vf10_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf11".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 11,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "pf0vf11_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf12".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 12,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "pf0vf12_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf0vf13".into(),
            iface_type: DpuServiceInterfaceTemplateType::Vf,
            pf_id: 0,
            vf_id: 13,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "pf0vf13_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "p1".into(),
            iface_type: DpuServiceInterfaceTemplateType::Physical,
            pf_id: 1,
            vf_id: 0,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "p1_if".into())]),
        },
        DpuServiceInterfaceTemplateDefinition {
            name: "pf1hpf".into(),
            iface_type: DpuServiceInterfaceTemplateType::Pf,
            pf_id: 1,
            vf_id: 0,
            chained_svc_if: Some(vec![(DOCA_HBN_SERVICE_NAME.into(), "pf1hpf_if".into())]),
        },
    ];
    interfaces
}

/// Builds the effective BF3/generic-BF4 interface inventory for one site configuration.
pub fn build_effective_dpu_interfaces(
    num_of_vfs: u32,
    intercept_bridging: Option<&DpfInterceptBridging>,
) -> Vec<DpuServiceInterfaceTemplateDefinition> {
    // Absence preserves the supported static inventory while projecting the hardware VF count.
    let Some(topology) = intercept_bridging else {
        return build_dpu_interfaces_vec()
            .into_iter()
            .filter(|interface| {
                !matches!(&interface.iface_type, DpuServiceInterfaceTemplateType::Vf)
                    || u32::try_from(interface.vf_id).is_ok_and(|vf_id| vf_id < num_of_vfs)
            })
            .collect();
    };

    // Configured intercept bridging replaces every ordinary PF/VF while retaining platform physical ports.
    let mut interfaces: Vec<_> = build_dpu_interfaces_vec()
        .into_iter()
        .filter(|interface| {
            matches!(
                &interface.iface_type,
                DpuServiceInterfaceTemplateType::Physical
            )
        })
        .collect();
    interfaces.extend(topology.interfaces().iter().map(|interface| {
        let identity = interface.identity;
        let name = identity.resource_name();
        let service_interface_stem = identity.service_interface_stem();
        let mut chained_svc_if = vec![
            (
                DOCA_HBN_SERVICE_NAME.to_string(),
                format!("{service_interface_stem}_if"),
            ),
            (
                DHCP_SERVER_SERVICE_NAME.to_string(),
                format!("d_{service_interface_stem}_if"),
            ),
        ];

        // PFs additionally expose FMDS; VFs must never receive that endpoint.
        if identity.vf_id.is_none() {
            chained_svc_if.push((
                FMDS_SERVICE_NAME.to_string(),
                format!("f_{service_interface_stem}_if"),
            ));
        }

        DpuServiceInterfaceTemplateDefinition {
            name,
            iface_type: DpuServiceInterfaceTemplateType::Patch(DpuServiceInterfacePatch {
                peer_bridge: interface.bridge.clone(),
                peer_patch_name: interface.patch_port.clone(),
                peer_external_ids: None,
            }),
            pf_id: i64::from(identity.pf_id),
            vf_id: i64::from(identity.vf_id.unwrap_or_default()),
            chained_svc_if: Some(chained_svc_if),
        }
    }));
    interfaces
}

/// Builds the static BF4 Astra interface inventory.
pub fn build_astra_dpu_interfaces_vec() -> Vec<DpuServiceInterfaceTemplateDefinition> {
    let mut interfaces = build_dpu_interfaces_vec();
    interfaces.extend(build_astra_patch_dpu_interfaces_vec());
    interfaces
}

/// Adds Astra's NICo-owned patch interfaces to a caller-provided inventory.
///
/// The patch names are reserved because the DPUDeployment service chains refer to them by name.
/// Accepting a caller-provided definition with one of those names could bind a chain to the wrong
/// interface type or peer bridge.
fn augment_astra_dpu_interfaces(
    mut interfaces: Vec<DpuServiceInterfaceTemplateDefinition>,
) -> Result<Vec<DpuServiceInterfaceTemplateDefinition>, DpfError> {
    for interface in build_astra_patch_dpu_interfaces_vec() {
        if let Some(existing) = interfaces
            .iter()
            .find(|existing| existing.name == interface.name)
        {
            if existing != &interface {
                return Err(DpfError::ConfigError(format!(
                    "Astra interface {} is reserved for NICo's CX/xplane patch topology and must use the canonical definition",
                    interface.name
                )));
            }
        } else {
            interfaces.push(interface);
        }
    }
    Ok(interfaces)
}

fn build_astra_patch_dpu_interfaces_vec() -> Vec<DpuServiceInterfaceTemplateDefinition> {
    astra_xplane_group_ids()
        .into_iter()
        .flat_map(|group_id| {
            [
                DpuServiceInterfaceTemplateDefinition {
                    name: format!("p-brcx-{group_id}-to-br-sfc"),
                    iface_type: DpuServiceInterfaceTemplateType::Patch(DpuServiceInterfacePatch {
                        peer_bridge: format!("brcx-{group_id}"),
                        peer_patch_name: String::new(),
                        peer_external_ids: None,
                    }),
                    pf_id: 0,
                    vf_id: 0,
                    chained_svc_if: None,
                },
                DpuServiceInterfaceTemplateDefinition {
                    name: format!("p-br-xplane-{group_id}-to-br-sfc"),
                    iface_type: DpuServiceInterfaceTemplateType::Patch(DpuServiceInterfacePatch {
                        peer_bridge: "br-xplane".to_string(),
                        peer_patch_name: String::new(),
                        peer_external_ids: Some(BTreeMap::from([
                            ("xplane".to_string(), "true".to_string()),
                            ("xplane-group-id".to_string(), group_id.to_string()),
                            ("xplane-downlink".to_string(), "patch".to_string()),
                        ])),
                    }),
                    pf_id: 0,
                    vf_id: 0,
                    chained_svc_if: None,
                },
            ]
        })
        .collect()
}

/// Calculates BF3 or generic-BF4 SF capacity from generated endpoints and additional capacity.
///
/// With intercept topology, `additional_managed_sf` increases the returned total. Without
/// topology, generated endpoints and additional capacity must fit inside `reserved`, which is the
/// returned legacy total.
pub fn calculate_pf_total_sf(
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
    intercept_bridging: Option<&DpfInterceptBridging>,
    reserved: u32,
    additional_managed_sf: u32,
) -> Result<u32, DpfError> {
    // HBN's chart supports at most 32 attached interfaces. Validate the rendered topology rather
    // than relying only on the configured one-PF/VF15 limit so custom public-SDK inventories
    // cannot bypass the service boundary.
    let hbn_interfaces = interfaces
        .iter()
        .flat_map(|interface| interface.chained_svc_if.iter().flatten())
        .filter(|(service, _)| service == DOCA_HBN_SERVICE_NAME)
        .count();
    if hbn_interfaces > MAX_HBN_SERVICE_INTERFACES {
        return Err(DpfError::ConfigError(format!(
            "configured DPF topology requires {hbn_interfaces} HBN interfaces, exceeding the supported maximum of {MAX_HBN_SERVICE_INTERFACES}"
        )));
    }

    // Configured inventory expands the SF pool by exactly the endpoints NICo asks DPF services to
    // consume. The reserved population remains available to DPF, firmware, and non-NICo users.
    let managed_endpoints = interfaces.iter().try_fold(0u32, |total, interface| {
        let interface_endpoints =
            u32::try_from(interface.chained_svc_if.as_ref().map_or(0, Vec::len)).map_err(|_| {
                DpfError::ConfigError("DPF service endpoint count exceeds u32".to_string())
            })?;
        total.checked_add(interface_endpoints).ok_or_else(|| {
            DpfError::ConfigError("DPF service endpoint count exceeds u32".to_string())
        })
    })?;
    let managed_sf_count = managed_endpoints
        .checked_add(additional_managed_sf)
        .ok_or_else(|| DpfError::ConfigError("DPF managed SF count exceeds u32".to_string()))?;

    // ROLLOUT COMPATIBILITY (DPU REPROVISIONING): inventory-free deployments must retain the
    // historical behavior where the configured reserved value is the complete PF_TOTAL_SF pool.
    // The managed SFs consume that pool rather than changing the flavor, but
    // must still fit inside it.
    if intercept_bridging.is_none() {
        if managed_sf_count > reserved {
            return Err(DpfError::ConfigError(format!(
                "configured DPF managed SFs ({managed_sf_count}) exceed the legacy \
                 dpf.pf_total_sf_reserved pool ({reserved})"
            )));
        }
        return Ok(reserved);
    }

    managed_sf_count.checked_add(reserved).ok_or_else(|| {
        DpfError::ConfigError(format!(
            "configured DPF managed SFs ({managed_sf_count}) plus \
             dpf.pf_total_sf_reserved ({reserved}) exceed u32"
        ))
    })
}

pub(crate) fn calculate_astra_pf_total_sf(
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
) -> Result<u32, DpfError> {
    // Astra has a fixed Weave DHCP Agent allocation and capacity headroom, but no
    // site-configurable reserve. Its capacity follows the actual NICo-managed endpoints.
    let managed_endpoints = interfaces.iter().try_fold(0u32, |total, interface| {
        let interface_endpoints =
            u32::try_from(interface.chained_svc_if.as_ref().map_or(0, Vec::len)).map_err(|_| {
                DpfError::ConfigError("DPF service endpoint count exceeds u32".to_string())
            })?;
        total.checked_add(interface_endpoints).ok_or_else(|| {
            DpfError::ConfigError("DPF service endpoint count exceeds u32".to_string())
        })
    })?;
    let managed_and_dhcp_agent = managed_endpoints
        .checked_add(DOCA_WEAVE_DHCP_AGENT_PF_TOTAL_SF)
        .ok_or_else(|| {
            DpfError::ConfigError(format!(
                "calculated Astra PF_TOTAL_SF plus DOCA Weave DHCP Agent PF SFs ({DOCA_WEAVE_DHCP_AGENT_PF_TOTAL_SF}) exceed u32"
            ))
        })?;
    managed_and_dhcp_agent
        .checked_add(PF_TOTAL_SF_BF4_ASTRA_FUDGE)
        .ok_or_else(|| {
            DpfError::ConfigError(format!(
                "calculated Astra PF_TOTAL_SF plus PF_TOTAL_SF_FUDGE ({PF_TOTAL_SF_BF4_ASTRA_FUDGE}) exceed u32"
            ))
        })
}

/// Validated initialization state that borrows caller-provided interfaces and owns SDK defaults.
struct ResolvedInitialization<'a> {
    interfaces: Cow<'a, [DpuServiceInterfaceTemplateDefinition]>,
    pf_total_sf: u32,
}

/// Resolves initialization interfaces and validates their SF capacity without writing resources.
fn resolve_initialization_inventory<'a>(
    config: &'a InitDpfResourcesConfig,
) -> Result<ResolvedInitialization<'a>, DpfError> {
    if config.num_of_vfs > MAX_BLUEFIELD_VFS_PER_PF {
        return Err(DpfError::ConfigError(format!(
            "DPF num_of_vfs must be <= {MAX_BLUEFIELD_VFS_PER_PF}"
        )));
    }

    // Astra's static interface inventory is safe only when deployment selectors isolate it from
    // BF3 and generic-BF4 nodes.
    if matches!(config.deployment_type, DpuDeploymentType::Bf4Astra)
        && !config.deployment_scoped_service_interfaces
    {
        return Err(DpfError::ConfigError(
            "BF4 Astra requires deployment_scoped_service_interfaces=true".to_string(),
        ));
    }
    if matches!(config.deployment_type, DpuDeploymentType::Bf4Astra)
        && config.additional_managed_sf != 0
    {
        return Err(DpfError::ConfigError(
            "BF4 Astra does not support additional managed SFs".to_string(),
        ));
    }

    // A normalized topology is valid only for the VF population supplied to its constructor.
    // Direct SDK callers can construct both inputs independently, so reject mismatches before
    // building a flavor or writing any initialization resource.
    if !matches!(config.deployment_type, DpuDeploymentType::Bf4Astra)
        && let Some(topology) = &config.intercept_bridging
        && topology.num_of_vfs() != config.num_of_vfs
    {
        return Err(DpfError::ConfigError(format!(
            "DPF intercept bridging was validated for num_of_vfs={}, but initialization requested num_of_vfs={}",
            topology.num_of_vfs(),
            config.num_of_vfs
        )));
    }

    let interfaces = if !matches!(config.deployment_type, DpuDeploymentType::Bf4Astra)
        && let Some(topology) = config.intercept_bridging.as_ref()
    {
        let projected = build_effective_dpu_interfaces(config.num_of_vfs, Some(topology));

        // Topology is the authoritative PF/VF inventory. Compare any explicit caller projection
        // with the canonical projection in order, reporting the first differing name so operators
        // can diagnose the mismatch. Accepting it would make flavor OVS state, DHCP ACLs,
        // ServiceInterfaces, and service chains disagree.
        if !config.interfaces.is_empty() && config.interfaces != projected {
            let first_mismatch = config
                .interfaces
                .iter()
                .zip(&projected)
                .find(|(received, expected)| received != expected)
                .map(|(received, expected)| {
                    (Some(received.name.as_str()), Some(expected.name.as_str()))
                })
                .or_else(|| {
                    config
                        .interfaces
                        .get(projected.len())
                        .map(|received| (Some(received.name.as_str()), None))
                })
                .or_else(|| {
                    projected
                        .get(config.interfaces.len())
                        .map(|expected| (None, Some(expected.name.as_str())))
                })
                .unwrap_or((None, None));
            return Err(DpfError::ConfigError(format!(
                "custom DPF interface inventory must match the effective intercept-bridging topology projection (first differing interface: received {}, expected {}; received {} interfaces, expected {})",
                first_mismatch.0.unwrap_or("<missing>"),
                first_mismatch.1.unwrap_or("<missing>"),
                config.interfaces.len(),
                projected.len(),
            )));
        }

        if config.interfaces.is_empty() {
            Cow::Owned(projected)
        } else {
            Cow::Borrowed(config.interfaces.as_slice())
        }
    } else if config.interfaces.is_empty() {
        // Astra retains its established static inventory and ignores site topology policy.
        Cow::Owned(match config.deployment_type {
            DpuDeploymentType::Bf4Astra => build_astra_dpu_interfaces_vec(),
            DpuDeploymentType::Bf3
            | DpuDeploymentType::Bf3Gb200
            | DpuDeploymentType::Bf4Generic => {
                build_effective_dpu_interfaces(config.num_of_vfs, None)
            }
        })
    } else if matches!(config.deployment_type, DpuDeploymentType::Bf4Astra) {
        Cow::Owned(augment_astra_dpu_interfaces(config.interfaces.clone())?)
    } else {
        Cow::Borrowed(config.interfaces.as_slice())
    };

    let pf_total_sf = match config.deployment_type {
        DpuDeploymentType::Bf4Astra => calculate_astra_pf_total_sf(interfaces.as_ref())?,
        DpuDeploymentType::Bf3 | DpuDeploymentType::Bf3Gb200 | DpuDeploymentType::Bf4Generic => {
            calculate_pf_total_sf(
                interfaces.as_ref(),
                config.intercept_bridging.as_ref(),
                config.pf_total_sf_reserved,
                config.additional_managed_sf,
            )?
        }
    };

    Ok(ResolvedInitialization {
        interfaces,
        pf_total_sf,
    })
}

/// Builds one DPUServiceInterface with an optional deployment suffix and node selector.
fn build_service_interface_with_scope(
    iface: &DpuServiceInterfaceTemplateDefinition,
    namespace: &str,
    suffix: &str,
    dpu_cluster_node_labels: Option<&BTreeMap<String, String>>,
) -> DPUServiceInterface {
    let (interface_type, physical, pf, vf, patch) = match &iface.iface_type {
        DpuServiceInterfaceTemplateType::Physical => (
            DpuServiceInterfaceTemplateSpecTemplateSpecInterfaceType::Physical,
            Some(DpuServiceInterfaceTemplateSpecTemplateSpecPhysical {
                interface_name: iface.name.clone(),
            }),
            None,
            None,
            None,
        ),
        DpuServiceInterfaceTemplateType::Pf => (
            DpuServiceInterfaceTemplateSpecTemplateSpecInterfaceType::Pf,
            None,
            Some(DpuServiceInterfaceTemplateSpecTemplateSpecPf {
                pf_id: iface.pf_id,
                virtual_network: None,
                // Preserve the legacy unscoped spec; controller selection belongs to the explicit
                // deployment-scoping migration and would otherwise reconcile existing resources.
                nic_selector: dpu_cluster_node_labels.is_some().then_some(
                    DpuServiceInterfaceTemplateSpecTemplateSpecPfNicSelector {
                        controller_number: Some(1),
                        pci: None,
                        r#type: DpuServiceInterfaceTemplateSpecTemplateSpecPfNicSelectorType::Dpu,
                    },
                ),
            }),
            None,
            None,
        ),
        DpuServiceInterfaceTemplateType::Vf => (
            DpuServiceInterfaceTemplateSpecTemplateSpecInterfaceType::Vf,
            None,
            None,
            Some(DpuServiceInterfaceTemplateSpecTemplateSpecVf {
                parent_interface_ref: Some(if iface.pf_id == 0 {
                    "p0".to_string()
                } else {
                    "p1".to_string()
                }),
                pf_id: iface.pf_id,
                vf_id: iface.vf_id,
                virtual_network: None,
                // Keep the VF selector symmetric with its parent PF and absent in legacy mode.
                nic_selector: dpu_cluster_node_labels.is_some().then_some(
                    DpuServiceInterfaceTemplateSpecTemplateSpecVfNicSelector {
                        controller_number: Some(1),
                        pci: None,
                        r#type: DpuServiceInterfaceTemplateSpecTemplateSpecVfNicSelectorType::Dpu,
                    },
                ),
            }),
            None,
        ),
        DpuServiceInterfaceTemplateType::Patch(patch) => (
            DpuServiceInterfaceTemplateSpecTemplateSpecInterfaceType::Patch,
            None,
            None,
            None,
            Some(DpuServiceInterfaceTemplateSpecTemplateSpecPatch {
                peer_bridge: patch.peer_bridge.clone(),
                peer_external_i_ds: patch.peer_external_ids.clone(),
                peer_patch_name: (!patch.peer_patch_name.is_empty())
                    .then(|| patch.peer_patch_name.clone()),
            }),
        ),
    };

    let resource_name = service_cr_name(&iface.name, suffix);
    let mut cr = DPUServiceInterface::new(
        &resource_name,
        DpuServiceInterfaceSpec {
            cluster_selector: None,
            template: DpuServiceInterfaceTemplate {
                metadata: None,
                spec: DpuServiceInterfaceTemplateSpec {
                    node_selector: dpu_cluster_node_labels.map(|labels| {
                        DpuServiceInterfaceTemplateSpecNodeSelector {
                            match_expressions: None,
                            match_labels: Some(labels.clone()),
                        }
                    }),
                    template: DpuServiceInterfaceTemplateSpecTemplate {
                        metadata: Some(DpuServiceInterfaceTemplateSpecTemplateMetadata {
                            annotations: None,
                            labels: Some(std::collections::BTreeMap::from([(
                                "interface".to_string(),
                                iface.name.clone(),
                            )])),
                        }),
                        spec: DpuServiceInterfaceTemplateSpecTemplateSpec {
                            interface_type,
                            node: None,
                            ovn: None,
                            pf,
                            physical,
                            service: None,
                            vf,
                            vlan: None,
                            patch,
                        },
                    },
                },
            },
            dpu_cluster_selector: None,
        },
    );
    cr.metadata = ObjectMeta {
        name: cr.metadata.name.clone(),
        namespace: Some(namespace.to_string()),
        ..Default::default()
    };
    cr
}

/// Builds the legacy unscoped DPUServiceInterface retained for compatibility.
pub fn build_service_interface(
    iface: &DpuServiceInterfaceTemplateDefinition,
    namespace: &str,
) -> DPUServiceInterface {
    build_service_interface_with_scope(iface, namespace, "", None)
}

/// Builds and applies deployment-scoped DPUServiceInterfaces in one pass.
async fn apply_service_interface_templates_with_scope<
    R: crate::repository::DpuServiceInterfaceRepository,
>(
    repo: &R,
    namespace: &str,
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
    suffix: &str,
    dpu_cluster_node_labels: Option<&BTreeMap<String, String>>,
) -> Result<(), crate::error::DpfError> {
    for iface in interfaces {
        let cr =
            build_service_interface_with_scope(iface, namespace, suffix, dpu_cluster_node_labels);
        crate::repository::DpuServiceInterfaceRepository::apply(repo, &cr).await?;
    }
    Ok(())
}

/// Builds and applies the legacy unscoped DPUServiceInterfaces in one pass.
pub async fn apply_service_interface_templates<
    R: crate::repository::DpuServiceInterfaceRepository,
>(
    repo: &R,
    namespace: &str,
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
) -> Result<(), crate::error::DpfError> {
    apply_service_interface_templates_with_scope(repo, namespace, interfaces, "", None).await
}

async fn create_flavor_services_and_deployment<
    R: DpuServiceTemplateRepository
        + DpuServiceConfigurationRepository
        + DpuDeploymentRepository
        + DpuFlavorRepository
        + DpuFlavorTemplateRepository
        + DpuServiceNADRepository
        + crate::repository::DpuServiceInterfaceRepository,
    L: ResourceLabeler,
>(
    repo: &R,
    namespace: &str,
    labeler: &L,
    services: &[ServiceDefinition],
    source: &DpuProvisioningSource,
    config: &InitDpfResourcesConfig,
    resolved: &ResolvedInitialization<'_>,
) -> Result<(), DpfError> {
    let deployment_type = config.deployment_type;
    let interfaces = resolved.interfaces.as_ref();
    let deployment_node_labels = labeler.node_labels_for_deployment_type(deployment_type)?;
    if config.deployment_scoped_service_interfaces && deployment_node_labels.is_empty() {
        return Err(DpfError::ConfigError(format!(
            "deployment-scoped initialization requires management-plane DPUNode labels for {deployment_type:?}"
        )));
    }

    let flavor_name = match deployment_type {
        DpuDeploymentType::Bf4Astra => {
            create_dpu_flavor_template(repo, namespace, config, resolved).await?
        }
        DpuDeploymentType::Bf3 | DpuDeploymentType::Bf3Gb200 | DpuDeploymentType::Bf4Generic => {
            create_dpu_flavor(repo, namespace, config, resolved).await?
        }
    };

    let interface_suffix = if config.deployment_scoped_service_interfaces {
        service_interface_cr_suffix(deployment_type)
    } else {
        ""
    };
    let dpu_cluster_node_labels = config
        .deployment_scoped_service_interfaces
        .then(|| dpu_cluster_node_selector(namespace, &config.deployment_name));

    // ROLLOUT SAFETY (DPF DATA PLANE):
    //
    // The disabled path must retain the legacy resource names and empty selectors so installing a
    // new NICo release does not trigger a scoping migration for existing BF3/BF4 interfaces. The
    // enabled path intentionally creates `-bf3`, `-bf3gb200`, `-bf4`, and `-astra` resources.
    //
    // LABEL-PLANE SAFETY: A DPUServiceInterface node selector is evaluated against Nodes in the
    // remote DPU cluster, not management-cluster DPUNode CRs. DPF propagates its canonical
    // `owned-by-dpudeployment=<namespace>_<deployment>` label to those remote Nodes, so scoped
    // interfaces must select that label. The deployment-class labels above remain exclusively for
    // management-plane DPUNode and DPUDeployment selection. Reusing them here matches zero remote
    // Nodes and prevents DPF from instantiating concrete ServiceInterfaces.
    //
    // Changing either names or selectors triggers DPF reconciliation and must remain an explicit
    // operator-controlled migration.
    //
    // Patch CRs require their peer bridge, so preserve flavor creation before interface templates.
    apply_service_interface_templates_with_scope(
        repo,
        namespace,
        interfaces,
        interface_suffix,
        dpu_cluster_node_labels.as_ref(),
    )
    .await?;

    // Each deployment gets its own service/NAD CRs (suffixed by deployment type)
    // so BF3 and BF4 do not overwrite each other's Helm values/versions in the
    // shared namespace. `nad_rename` maps each deployment-local NAD name to its
    // suffixed CR name so service configurations reference the right NAD.
    let suffix = deployment_cr_suffix(deployment_type);
    let nad_rename: BTreeMap<String, String> = services
        .iter()
        .filter_map(|svc| svc.service_nad.as_ref())
        .map(|nad| (nad.name.clone(), service_cr_name(&nad.name, suffix)))
        .collect();

    for svc in services {
        DpuServiceTemplateRepository::apply(repo, &build_service_template(svc, namespace, suffix))
            .await?;
        DpuServiceConfigurationRepository::apply(
            repo,
            &build_service_configuration(svc, namespace, suffix, &nad_rename),
        )
        .await?;
        if let Some(nad) = build_service_nad(svc, namespace, suffix).as_ref() {
            DpuServiceNADRepository::apply(repo, nad).await?;
        }
    }

    let deployment = build_deployment(
        services,
        &config.deployment_name,
        source,
        &flavor_name,
        namespace,
        interfaces,
        deployment_node_labels,
        deployment_type,
    );
    DpuDeploymentRepository::apply(repo, &deployment).await?;
    Ok(())
}

impl<
    R: BfbRepository
        + BlueFieldSoftwareRepository
        + DpuFlavorRepository
        + DpuFlavorTemplateRepository
        + DpuDeploymentRepository
        + DpuServiceTemplateRepository
        + DpuServiceConfigurationRepository
        + DpuServiceNADRepository
        + crate::repository::DpuServiceInterfaceRepository
        + K8sConfigRepository
        + DpfOperatorConfigRepository,
    L: ResourceLabeler,
> DpfSdk<R, L>
{
    /// Create all initialization CRDs for the "Provision a DPU" flow.
    ///
    /// Order: provisioning source (BFB for BF3, or BlueFieldSoftware for BF4 —
    /// the controller downloads either), DPUFlavor, DPUDeployment with
    /// `dpu_sets` referencing the source and DPUFlavor. The operator then
    /// creates DPU objects and drives provisioning.
    ///
    /// See: https://docs.nvidia.com/networking/display/dpf2507/component+description#ProvisionaDPU
    pub async fn create_initialization_objects(
        &self,
        config: &InitDpfResourcesConfig,
    ) -> Result<(), DpfError> {
        // Keep validation here for split-phase callers that construct the SDK separately.
        let resolved = resolve_initialization_inventory(config)?;
        self.create_initialization_objects_resolved(config, resolved)
            .await
    }

    /// Applies initialization resources from state that has already passed pure preflight.
    async fn create_initialization_objects_resolved(
        &self,
        config: &InitDpfResourcesConfig,
        resolved: ResolvedInitialization<'_>,
    ) -> Result<(), DpfError> {
        validate_bf4_astra_ra2_2_runtime_configmap(
            &*self.repo,
            &self.namespace,
            config.deployment_type,
        )
        .await?;

        let source = match &config.bluefield_software {
            Some(params) => DpuProvisioningSource::BlueFieldSoftware(
                create_bluefield_software(&*self.repo, &self.namespace, params).await?,
            ),
            None => DpuProvisioningSource::Bfb(
                create_bfb(&*self.repo, &self.namespace, &config.bfb_url).await?,
            ),
        };
        let services = if config.services.is_empty() {
            crate::services::default_services(&crate::services::ServiceRegistryConfig::default())
        } else {
            config.services.clone()
        };
        // Before the flavor: it references these with `optional` unset, so a DPU
        // instantiated in between would point at a ConfigMap that does not exist.
        create_extra_script_configmaps(&*self.repo, &self.namespace, config.deployment_type)
            .await?;
        create_flavor_services_and_deployment(
            &*self.repo,
            &self.namespace,
            &self.labeler,
            &services,
            &source,
            config,
            &resolved,
        )
        .await?;

        Ok(())
    }
}

impl<R: DpuDeploymentRepository, L> DpfSdk<R, L> {
    /// Update the BFB reference in a DPUDeployment.
    ///
    /// Patches the deployment to point to the given BFB name.
    /// The BFB CR must already exist.
    pub async fn update_deployment_bfb(
        &self,
        deployment_name: &str,
        bfb_name: &str,
    ) -> Result<(), DpfError> {
        let patch = json!({
            "spec": {
                "dpus": {
                    "bfb": bfb_name
                }
            }
        });
        DpuDeploymentRepository::patch(&*self.repo, deployment_name, &self.namespace, patch).await
    }
}

impl<R: DpuServiceRepository, L> DpfSdk<R, L> {
    /// Get any observed DPUService from this SDK's namespace.
    pub async fn get_dpu_service(
        &self,
        service_name: &str,
    ) -> Result<Option<DpuServiceObservation>, DpfError> {
        DpuServiceRepository::get(&*self.repo, service_name, &self.namespace)
            .await?
            .map(dpu_service_from_resource)
            .transpose()
    }

    /// Create a direct, detached DPUService in this SDK's namespace.
    ///
    /// The caller decides how to handle an `AlreadyExists` response, because
    /// the object must be checked for its particular ownership contract.
    pub async fn create_dpu_service(
        &self,
        service: &DetachedDpuServiceDefinition,
    ) -> Result<DpuServiceObservation, DpfError> {
        let mut resource = dpu_service_to_resource(service);
        resource.metadata.namespace = Some(self.namespace.clone());
        let created = DpuServiceRepository::create(&*self.repo, &resource).await?;
        dpu_service_from_resource(created)
    }

    /// Merge-patch a detached DPUService in this SDK's namespace.
    pub async fn patch_dpu_service(
        &self,
        service_name: &str,
        patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        DpuServiceRepository::patch(&*self.repo, service_name, &self.namespace, patch).await
    }

    /// Delete a detached DPUService in this SDK's namespace.
    pub async fn delete_dpu_service(&self, service_name: &str) -> Result<(), DpfError> {
        DpuServiceRepository::delete(&*self.repo, service_name, &self.namespace).await
    }
}

/// Convert the SDK-owned detached-service definition at the SDK/repository
/// boundary.  DPF's generated type must not escape this module.
fn dpu_service_to_resource(service: &DetachedDpuServiceDefinition) -> DPUService {
    DPUService {
        metadata: ObjectMeta {
            name: Some(service.name.clone()),
            namespace: Some(service.namespace.clone()),
            labels: (!service.labels.is_empty()).then(|| service.labels.clone()),
            ..Default::default()
        },
        spec: DpuServiceSpec {
            config_ports: None,
            deploy_in_cluster: Some(service.deploy_in_cluster),
            dpu_cluster_selector: None,
            helm_chart: DpuServiceHelmChart {
                source: DpuServiceHelmChartSource {
                    chart: Some(service.helm_chart.chart.clone()),
                    path: None,
                    release_name: Some(service.helm_chart.release_name.clone()),
                    repo_url: service.helm_chart.repo_url.clone(),
                    version: service.helm_chart.version.clone(),
                },
                values: service.helm_chart.values.clone(),
            },
            interfaces: None,
            paused: None,
            security: Some(DpuServiceSecurity {
                privileged: Some(service.security_privileged),
            }),
            service_daemon_set: Some(DpuServiceServiceDaemonSet {
                annotations: None,
                labels: None,
                node_selector: Some(detached_node_selector(&service.node_selector_labels)),
                resources: None,
                update_strategy: None,
            }),
            service_id: None,
        },
        status: None,
    }
}

/// Convert a DPF-generated DPUService into the SDK-owned view needed by the
/// controller.  The repository remains the only layer that deals in checked
/// DPF CR types.
fn dpu_service_from_resource(service: DPUService) -> Result<DpuServiceObservation, DpfError> {
    Ok(DpuServiceObservation {
        name: service.metadata.name,
        namespace: service.metadata.namespace,
        labels: service.metadata.labels.unwrap_or_default(),
        helm_chart: DpuServiceHelmChartObservation {
            repo_url: service.spec.helm_chart.source.repo_url,
            chart: service.spec.helm_chart.source.chart,
            version: service.spec.helm_chart.source.version,
            release_name: service.spec.helm_chart.source.release_name,
            values: service.spec.helm_chart.values,
        },
        deploy_in_cluster: service.spec.deploy_in_cluster,
        dpu_cluster_selector_present: service.spec.dpu_cluster_selector.is_some(),
        interfaces_present: service.spec.interfaces.is_some(),
        paused: service.spec.paused,
        security_privileged: service
            .spec
            .security
            .and_then(|security| security.privileged),
        service_daemon_set_node_selector: service
            .spec
            .service_daemon_set
            .as_ref()
            .and_then(|daemon_set| daemon_set.node_selector.as_ref())
            .map(serde_json::to_value)
            .transpose()?,
        service_id: service.spec.service_id,
        config_ports_present: service.spec.config_ports.is_some(),
        is_deleting: service.metadata.deletion_timestamp.is_some(),
    })
}

fn detached_node_selector(
    labels: &BTreeMap<String, String>,
) -> DpuServiceServiceDaemonSetNodeSelector {
    DpuServiceServiceDaemonSetNodeSelector {
        node_selector_terms: vec![DpuServiceServiceDaemonSetNodeSelectorNodeSelectorTerms {
            match_expressions: Some(
                labels
                    .iter()
                    .map(|(key, value)| {
                        DpuServiceServiceDaemonSetNodeSelectorNodeSelectorTermsMatchExpressions {
                            key: key.clone(),
                            operator: "In".to_owned(),
                            values: Some(vec![value.clone()]),
                        }
                    })
                    .collect(),
            ),
            match_fields: None,
        }],
    }
}

impl<R: DpuDeviceRepository, L: ResourceLabeler> DpfSdk<R, L> {
    /// Merge changes into a DPUDevice's DPU-cluster node labels.
    ///
    /// `dpu_device_name` is the raw device ID (without the `device-` CR
    /// prefix); the SDK applies the prefix internally. `Some(value)` adds or
    /// replaces a label, while `None` removes it. Existing node labels and
    /// cluster fields not named by `changes` are preserved.
    pub async fn merge_dpu_device_node_labels(
        &self,
        dpu_device_name: &str,
        changes: BTreeMap<String, Option<String>>,
    ) -> Result<(), DpfError> {
        let cr_name = dpu_device_cr_name(dpu_device_name);
        let patch = json!({
            "spec": {
                "cluster": {
                    "nodeLabels": changes,
                },
            },
        });
        DpuDeviceRepository::patch(&*self.repo, &cr_name, &self.namespace, patch).await
    }

    /// Returns the DPU-cluster node labels on one DPUDevice CR.
    pub async fn get_dpu_device_node_labels(
        &self,
        dpu_device_name: &str,
    ) -> Result<BTreeMap<String, String>, DpfError> {
        let cr_name = dpu_device_cr_name(dpu_device_name);
        let device = DpuDeviceRepository::get(&*self.repo, &cr_name, &self.namespace)
            .await?
            .ok_or_else(|| DpfError::not_found("DPUDevice", &cr_name))?;
        Ok(device
            .spec
            .cluster
            .and_then(|cluster| cluster.node_labels)
            .unwrap_or_default())
    }

    /// Register a new DPU device.
    ///
    /// This operation is idempotent - if the device already exists, it will be
    /// skipped. This handles state machine retries gracefully.
    pub async fn register_dpu_device(
        &self,
        info: DpuDeviceInfo,
        astra_nics: Option<Vec<&DpaInterface>>,
    ) -> Result<(), DpfError> {
        let cr_name = dpu_device_cr_name(&info.device_id);

        // Values are supplied only on creation. In particular, do not require
        // a complete Astra NIC snapshot when this is an idempotent retry for
        // an existing DPUDevice.
        if let Some(existing) =
            DpuDeviceRepository::get(&*self.repo, &cr_name, &self.namespace).await?
        {
            if existing.metadata.deletion_timestamp.is_some() {
                return Err(DpfError::InvalidState(format!(
                    "DPUDevice {cr_name} is being deleted (has deletionTimestamp); \
                     cannot re-register until the old resource is fully removed"
                )));
            }
            if existing.spec.values.is_none()
                && let Some(nics) = astra_nics.as_ref()
            {
                let values = astra_underlay_configuration(&cr_name, nics)?;
                DpuDeviceRepository::patch(
                    &*self.repo,
                    &cr_name,
                    &self.namespace,
                    json!({ "spec": { "values": values } }),
                )
                .await?;
                tracing::info!(device_name = %cr_name, "Backfilled Astra DPU device values");
                return Ok(());
            }
            tracing::debug!(device_name = %cr_name, "DPU device already exists");
            return Ok(());
        }

        // Build values field from astra_nics configuration passed in.
        let values = match astra_nics {
            Some(nics) => Some(astra_underlay_configuration(&cr_name, &nics)?),
            None => None,
        };

        tracing::info!(device_name = %cr_name, "Registering DPU device");

        let device = DPUDevice {
            metadata: ObjectMeta {
                name: Some(cr_name.clone()),
                namespace: Some(self.namespace.clone()),
                labels: {
                    let labels = self.labeler.device_labels(&info);
                    if labels.is_empty() {
                        None
                    } else {
                        Some(labels)
                    }
                },
                ..Default::default()
            },
            spec: DpuDeviceSpec {
                bmc_ip: Some(info.dpu_bmc_ip.to_string()),
                bmc_port: Some(443),
                number_of_p_fs: Some(1),
                opn: None,
                pf0_name: None,
                psid: None,
                serial_number: info.serial_number,
                bmc_credential_secret_name: None,
                cluster: None,
                nic_device_count: None,
                values,
            },
            status: None,
        };

        match DpuDeviceRepository::create(&*self.repo, &device).await {
            Ok(_) => {
                tracing::info!(device_name = %cr_name, "Created DPU device");
                Ok(())
            }
            Err(DpfError::KubeError(kube::Error::Api(ref err)))
                if err.is_already_exists() || err.is_conflict() =>
            {
                let existing =
                    DpuDeviceRepository::get(&*self.repo, &cr_name, &self.namespace).await?;
                if existing
                    .as_ref()
                    .is_some_and(|d| d.metadata.deletion_timestamp.is_some())
                {
                    return Err(DpfError::InvalidState(format!(
                        "DPUDevice {cr_name} is being deleted (has deletionTimestamp); \
                         cannot re-register until the old resource is fully removed"
                    )));
                }
                tracing::debug!(device_name = %cr_name, "DPU device already exists (concurrent create)");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Delete a DPU device. `dpu_device_name` is the raw device ID (without
    /// the `device-` CR prefix); the SDK applies the prefix internally.
    pub async fn delete_dpu_device(&self, dpu_device_name: &str) -> Result<(), DpfError> {
        let cr_name = dpu_device_cr_name(dpu_device_name);
        DpuDeviceRepository::delete(&*self.repo, &cr_name, &self.namespace).await
    }
}

/// Builds the Astra DPUDevice values from its eight ordered underlay NICs.
fn astra_underlay_configuration(
    device_name: &str,
    astra_nics: &[&DpaInterface],
) -> Result<BTreeMap<String, serde_json::Value>, DpfError> {
    let underlay_ip_macs = astra_nics
        .iter()
        .enumerate()
        .map(|(index, nic)| {
            let ip = match nic.underlay_ip {
                Some(IpAddr::V4(ip)) => Ok(ip),
                Some(IpAddr::V6(ip)) => Err(DpfError::ConfigError(format!(
                    "Astra underlay NIC {index} has unsupported IPv6 address {ip}; expected IPv4"
                ))),
                None => Err(DpfError::ConfigError(format!(
                    "Astra underlay NIC {index} has no underlay IP"
                ))),
            }?;
            Ok((nic.mac_address.to_string(), ip))
        })
        .collect::<Result<Vec<_>, DpfError>>()?;
    let values = astra_underlay_values_for_ip_macs(&underlay_ip_macs)?;
    let underlay_ip_mac_strings: Vec<String> = underlay_ip_macs
        .iter()
        .map(|(_, ip)| ip.to_string())
        .collect();
    tracing::info!(
        "Setup DPUDevice {device_name} values for Astra with underlay_ip_macs={} (mask=/31, routes=/16,/13)",
        underlay_ip_mac_strings.join(", ")
    );

    Ok(values)
}

/// Build the per-DPU values field for the DPU device object. This
/// information is for consumption by the BF4 Astra `DPUFlavorTemplate`.
/// Input order does not matter, the BF4 Astra template uses the MAC address
/// to find the matching PCI device and bridge at runtime.
/// For each input index `N`, `ip_N_val` as a `/31` address, `gw_N_val`
/// `route1_N_val` (/16 route) `route2_N_val` (/13 route) and `mac_N_val`
/// are added to the values field.
fn astra_underlay_values_for_ip_macs(
    underlay_ip_macs: &[(String, Ipv4Addr)],
) -> Result<BTreeMap<String, serde_json::Value>, DpfError> {
    // Astra has four rails and two switch planes. This documents the required set of slots; the
    // input pair order is intentionally not tied to this array.
    const RAIL_SWITCH_PLANES: [(u8, u8); 8] = [
        (0, 0),
        (1, 0),
        (2, 0),
        (3, 0),
        (0, 1),
        (1, 1),
        (2, 1),
        (3, 1),
    ];

    // Make sure that there are 8 MAC-IP pairs and they are all unique.
    if underlay_ip_macs.len() != RAIL_SWITCH_PLANES.len() {
        tracing::error!(
            expected_underlay_ip_count = RAIL_SWITCH_PLANES.len(),
            actual_underlay_ip_count = underlay_ip_macs.len(),
            "Astra requires exactly eight underlay MAC/IP pairs"
        );
        return Err(DpfError::ConfigError(format!(
            "Astra requires exactly {} underlay MAC/IP pairs, got {}",
            RAIL_SWITCH_PLANES.len(),
            underlay_ip_macs.len()
        )));
    }
    let unique_underlay_ip_count = underlay_ip_macs
        .iter()
        .map(|(_, ip)| *ip)
        .collect::<BTreeSet<_>>()
        .len();
    if unique_underlay_ip_count != underlay_ip_macs.len() {
        tracing::error!(
            underlay_ip_count = underlay_ip_macs.len(),
            unique_underlay_ip_count,
            "Astra underlay IPs must be unique"
        );
        return Err(DpfError::ConfigError(
            "Astra underlay IPs must be unique".to_string(),
        ));
    }
    let unique_underlay_mac_count = underlay_ip_macs
        .iter()
        .map(|(mac, _)| mac)
        .collect::<BTreeSet<_>>()
        .len();
    if unique_underlay_mac_count != underlay_ip_macs.len() {
        tracing::error!(
            underlay_mac_count = underlay_ip_macs.len(),
            unique_underlay_mac_count,
            "Astra underlay MACs must be unique"
        );
        return Err(DpfError::ConfigError(
            "Astra underlay MACs must be unique".to_string(),
        ));
    }

    // Add ip_N_val, gw_N_val, route1_N_val, route2_N_val and mac_N_val
    // to the values field, which will be added to the DPU device object.
    // N goes from 0 to 7, one for each of the input ip-mac pairs.
    let mut values = BTreeMap::new();
    for (index, (mac, ip)) in underlay_ip_macs.iter().enumerate() {
        let octets = ip.octets();
        let gateway = Ipv4Addr::from(u32::from(*ip) ^ 1);
        values.insert(format!("ip_{index}_val"), json!(format!("{ip}/31")));
        values.insert(format!("gw_{index}_val"), json!(gateway.to_string()));
        values.insert(
            format!("route1_{index}_val"),
            json!(format!("{}.{}.0.0/16", octets[0], octets[1])),
        );
        values.insert(
            format!("route2_{index}_val"),
            json!(format!("{}.{}.0.0/13", octets[0], octets[1] & 0b1111_1000)),
        );
        values.insert(format!("mac_{index}_val"), json!(mac.clone()));
    }
    Ok(values)
}

impl<R: DpuNodeRepository, L: ResourceLabeler> DpfSdk<R, L> {
    /// Register a new DPU node (host with DPUs).
    ///
    /// This operation is idempotent - if the node already exists, it will be
    /// updated with the new configuration. This is important for multi-DPU setups
    /// where multiple concurrent state machine invocations may call this method.
    pub async fn register_dpu_node(&self, info: DpuNodeInfo) -> Result<(), DpfError> {
        let node_name = dpu_node_cr_name(&info.node_id);

        let node = DPUNode {
            metadata: ObjectMeta {
                name: Some(node_name.clone()),
                namespace: Some(self.namespace.clone()),
                labels: {
                    let mut labels = self
                        .labeler
                        .node_labels_for_deployment_type(info.deployment_type)?;
                    labels.extend(self.labeler.node_context_labels(&info));
                    if labels.is_empty() {
                        None
                    } else {
                        Some(labels)
                    }
                },
                ..Default::default()
            },
            spec: DpuNodeSpec {
                dpus: Some(
                    info.device_ids
                        .into_iter()
                        .map(|id| DpuNodeDpus {
                            name: dpu_device_cr_name(&id),
                        })
                        .collect(),
                ),
                node_dms_address: None,
                node_reboot_method: Some(DpuNodeNodeRebootMethod {
                    external: Some(DpuNodeNodeRebootMethodExternal {}),
                    g_noi: None,
                    host_agent: None,
                    script: None,
                    none: None,
                }),
            },
            status: None,
        };

        match DpuNodeRepository::create(&*self.repo, &node).await {
            Ok(_) => {
                tracing::info!(node = %node_name, "Created DPU node");
                Ok(())
            }
            Err(DpfError::KubeError(kube::Error::Api(ref err)))
                if err.is_already_exists() || err.is_conflict() =>
            {
                let existing =
                    DpuNodeRepository::get(&*self.repo, &node_name, &self.namespace).await?;
                if existing
                    .as_ref()
                    .is_some_and(|n| n.metadata.deletion_timestamp.is_some())
                {
                    return Err(DpfError::InvalidState(format!(
                        "DPUNode {node_name} is being deleted (has deletionTimestamp); \
                         cannot re-register until the old resource is fully removed"
                    )));
                }
                tracing::debug!(node = %node_name, "DPU node already exists (concurrent create)");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Check that a DPUNode's labels contain all entries from the current
    /// labeler's `node_labels()`. Returns `false` when the node exists but
    /// has stale labels (e.g. from a previous label version). Returns `true`
    /// when the node does not exist yet.
    pub async fn verify_node_labels(
        &self,
        node_name: &str,
        deployment_type: DpuDeploymentType,
    ) -> Result<bool, DpfError> {
        let node = DpuNodeRepository::get(&*self.repo, node_name, &self.namespace).await?;

        let Some(node) = node else {
            return Ok(true);
        };

        let required_labels = self
            .labeler
            .node_labels_for_deployment_type(deployment_type)?;
        let node_labels = node.metadata.labels.as_ref();

        Ok(required_labels.iter().all(|(key, required_value)| {
            node_labels.is_some_and(|labels| {
                labels
                    .get(key)
                    .is_some_and(|node_value| node_value == required_value)
            })
        }))
    }

    /// Moves one DPUNode from its source DPUDeployment selector to its target
    /// selector.
    ///
    /// Labels shared by both deployments and labels outside either selector
    /// are preserved. The transfer uses the DPUNode's `resourceVersion`, so a
    /// concurrent update returns a Kubernetes conflict instead of being
    /// overwritten. Repeating a completed transfer does nothing. A DPUNode that
    /// matches neither selector is rejected rather than assigned to the target
    /// deployment.
    pub async fn transfer_dpu_node_deployment_labels(
        &self,
        node_name: &str,
        source_deployment_type: DpuDeploymentType,
        target_deployment_type: DpuDeploymentType,
    ) -> Result<(), DpfError> {
        let node = DpuNodeRepository::get(&*self.repo, node_name, &self.namespace)
            .await?
            .ok_or_else(|| DpfError::not_found("DPUNode", node_name))?;
        let source_labels = self
            .labeler
            .node_labels_for_deployment_type(source_deployment_type)?;
        let target_labels = self
            .labeler
            .node_labels_for_deployment_type(target_deployment_type)?;
        if source_labels.is_empty() || target_labels.is_empty() || source_labels == target_labels {
            return Err(DpfError::ConfigError(format!(
                "deployment label transfer requires distinct, nonempty selectors for \
                 {source_deployment_type:?} and {target_deployment_type:?}"
            )));
        }
        let current_labels = node.metadata.labels.unwrap_or_default();
        let matches_selector = |selector: &BTreeMap<String, String>| {
            selector
                .iter()
                .all(|(key, value)| current_labels.get(key) == Some(value))
        };
        let matches_source = matches_selector(&source_labels);
        let matches_target = matches_selector(&target_labels);
        if !matches_source && !matches_target {
            return Err(DpfError::InvalidState(format!(
                "DPUNode {node_name} labels match neither the {source_deployment_type:?} nor the \
                 {target_deployment_type:?} deployment selector"
            )));
        }

        let has_source_only_label = source_labels
            .keys()
            .any(|key| !target_labels.contains_key(key) && current_labels.contains_key(key));
        let target_selector_differs = target_labels
            .iter()
            .any(|(key, value)| current_labels.get(key) != Some(value));
        if !has_source_only_label && !target_selector_differs {
            return Ok(());
        }

        let resource_version = node.metadata.resource_version.ok_or_else(|| {
            DpfError::InvalidState(format!(
                "DPUNode {node_name} has no resourceVersion for deployment label transfer"
            ))
        })?;
        let mut label_changes: serde_json::Map<String, serde_json::Value> = source_labels
            .keys()
            .filter(|key| !target_labels.contains_key(*key))
            .map(|key| (key.clone(), serde_json::Value::Null))
            .collect();
        label_changes.extend(
            target_labels
                .into_iter()
                .map(|(key, value)| (key, serde_json::Value::String(value))),
        );

        // The transfer must not leave the DPUNode matching both deployment
        // selectors. One merge patch makes the removal and addition atomic,
        // while `resourceVersion` prevents this read from overwriting a
        // concurrent DPUNode update.
        let patch = json!({
            "metadata": {
                "resourceVersion": resource_version,
                "labels": label_changes,
            }
        });
        DpuNodeRepository::patch(&*self.repo, node_name, &self.namespace, patch).await
    }

    /// Check if reboot is required for a DPU node.
    pub async fn is_reboot_required(&self, node_name: &str) -> Result<bool, DpfError> {
        let node = DpuNodeRepository::get(&*self.repo, node_name, &self.namespace).await?;

        let Some(node) = node else {
            return Err(DpfError::not_found("DPUNode", node_name));
        };

        let Some(annotations) = node.metadata.annotations else {
            return Ok(false);
        };

        Ok(annotations.contains_key(RESTART_ANNOTATION))
    }

    /// Clear the reboot required annotation.
    pub async fn reboot_complete(&self, node_name: &str) -> Result<(), DpfError> {
        let patch = json!({
            "metadata": {
                "annotations": {
                    RESTART_ANNOTATION: null
                }
            }
        });
        DpuNodeRepository::patch(&*self.repo, node_name, &self.namespace, patch).await
    }

    /// Delete a DPU node and associated resources.
    pub async fn delete_dpu_node(&self, node_name: &str) -> Result<(), DpfError> {
        let patch = self.node_label_removal_patch();
        if let Err(e) =
            DpuNodeRepository::patch(&*self.repo, node_name, &self.namespace, patch).await
        {
            tracing::warn!(node_name, error = %e, "Failed to remove label from DPU node");
        }

        DpuNodeRepository::delete(&*self.repo, node_name, &self.namespace).await
    }
}

impl<R: DpuRepository, L> DpfSdk<R, L> {
    /// Get the DPU phase for a specific DPU.
    pub async fn get_dpu_phase(
        &self,
        dpu_device_name: &str,
        node_name: &str,
    ) -> Result<DpuPhase, DpfError> {
        let dpf_id = node_id_from_dpu_node_cr_name(node_name);
        let cr_name = dpu_cr_name(dpu_device_name, dpf_id);
        let dpu = DpuRepository::get(&*self.repo, &cr_name, &self.namespace).await?;

        let Some(dpu) = dpu else {
            return Err(DpfError::not_found("DPU", cr_name));
        };

        // A DPU being torn down (e.g. right after reprovision deleted it) still reports
        // its old status.phase (often Ready) until the operator's finalizer runs. Treat
        // a set deletionTimestamp as authoritative so callers never act on the stale phase.
        if dpu.metadata.deletion_timestamp.is_some() {
            return Ok(DpuPhase::Deleting);
        }

        let Some(status) = dpu.status else {
            return Err(DpfError::InvalidState(format!(
                "DPU {cr_name} has no status"
            )));
        };

        Ok(DpuPhase::from(status.phase))
    }

    /// Reprovision a DPU by deleting the DPU CR.
    ///
    /// In the DPUDeployment (M4) model the operator creates DPU from DPUDevice; deleting the DPU
    /// CR causes the operator to remove it and create a new DPU (same name) that waits on node
    /// effect. The DPUDevice CR is left in place. A missing DPU CR means deletion is already
    /// complete. Treating that as success keeps retries safe when an earlier attempt deleted the
    /// DPU but did not persist its next state, and when a DPUSet deletes it during label transfer.
    pub async fn reprovision_dpu(
        &self,
        dpu_device_name: &str,
        node_name: &str,
    ) -> Result<(), DpfError> {
        let dpf_id = node_id_from_dpu_node_cr_name(node_name);
        let cr_name = dpu_cr_name(dpu_device_name, dpf_id);
        match DpuRepository::delete(&*self.repo, &cr_name, &self.namespace).await {
            Ok(()) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl<R: DpuRepository + DpuDeploymentRepository, L: ResourceLabeler> DpfSdk<R, L> {
    /// Read every requested DPU phase only when one deployment owns the full set.
    ///
    /// A DPU that is not Ready may not report its installed BFB yet, so ownership
    /// is sufficient until that phase. A Ready DPU must also match the flavor and
    /// provisioning source declared by the deployment. `None` means at least one
    /// requested DPU is missing, is being deleted, belongs to another deployment,
    /// has no status yet, or does not match the Ready configuration.
    pub async fn get_dpu_phases_for_deployment_type(
        &self,
        dpu_device_names: &[String],
        node_name: &str,
        deployment_type: DpuDeploymentType,
    ) -> Result<Option<BTreeMap<String, DpuPhase>>, DpfError> {
        let (deployment_name, deployment) = self.deployment_for_type(deployment_type).await?;
        if !dpu_deployment_is_ready(&deployment) {
            return Ok(None);
        }
        if deployment.spec.dpus.flavor.is_none() {
            return Err(DpfError::InvalidState(format!(
                "DPUDeployment {deployment_name} uses a DPUFlavorTemplate, which cannot be \
                 compared with DPU.spec.dpuFlavor"
            )));
        }

        let expected_owner = dpu_deployment_owner_label_value(&self.namespace, &deployment_name);
        let owner_selector = format!("{DPU_OWNED_BY_DEPLOYMENT_LABEL}={expected_owner}");
        let dpf_id = node_id_from_dpu_node_cr_name(node_name);
        let mut dpus_by_name =
            DpuRepository::list(&*self.repo, &self.namespace, Some(&owner_selector))
                .await?
                .into_iter()
                .filter_map(|dpu| Some((dpu.metadata.name.clone()?, dpu)))
                .collect::<HashMap<_, _>>();
        let mut phases = BTreeMap::new();

        for dpu_device_name in dpu_device_names {
            let cr_name = dpu_cr_name(dpu_device_name, dpf_id);
            let Some(dpu) = dpus_by_name.remove(&cr_name) else {
                return Ok(None);
            };
            let has_expected_owner = dpu
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(DPU_OWNED_BY_DEPLOYMENT_LABEL))
                == Some(&expected_owner);
            if !has_expected_owner || dpu.metadata.deletion_timestamp.is_some() {
                return Ok(None);
            }

            let Some(status) = dpu.status.as_ref() else {
                return Ok(None);
            };
            let phase = DpuPhase::from(status.phase.clone());
            if phase == DpuPhase::Ready {
                match dpu_comparison(&self.namespace, &dpu, &deployment) {
                    DpuComparison::Match => {}
                    DpuComparison::Mismatch(mismatch) => {
                        return Err(DpfError::InvalidState(format!(
                            "Ready DPU {} does not match DPUDeployment {deployment_name}; expected provisioning source {}",
                            mismatch.dpu_cr_name, mismatch.target_source,
                        )));
                    }
                    DpuComparison::Inconclusive => {
                        return Err(DpfError::InvalidState(format!(
                            "DPU {cr_name} cannot be compared with DPUDeployment {deployment_name}"
                        )));
                    }
                }
            }
            phases.insert(dpu_device_name.clone(), phase);
        }

        Ok(Some(phases))
    }

    /// Delete source deployment DPU CRs without deleting target replacements.
    ///
    /// Each delete carries the observed DPU UID as a Kubernetes precondition.
    /// If a source DPU disappears and a target DPU reuses its deterministic name,
    /// a retry preserves the replacement. A DPU owned by any deployment other
    /// than the declared source or target is rejected.
    pub async fn delete_source_dpus_for_deployment_migration(
        &self,
        dpu_device_names: &[String],
        node_name: &str,
        source_deployment_type: DpuDeploymentType,
        target_deployment_type: DpuDeploymentType,
    ) -> Result<(), DpfError> {
        let (source_deployment_name, _) = self.deployment_for_type(source_deployment_type).await?;
        let (target_deployment_name, _) = self.deployment_for_type(target_deployment_type).await?;
        let source_owner =
            dpu_deployment_owner_label_value(&self.namespace, &source_deployment_name);
        let target_owner =
            dpu_deployment_owner_label_value(&self.namespace, &target_deployment_name);
        let dpf_id = node_id_from_dpu_node_cr_name(node_name);
        let mut dpus_by_name = DpuRepository::list(&*self.repo, &self.namespace, None)
            .await?
            .into_iter()
            .filter_map(|dpu| Some((dpu.metadata.name.clone()?, dpu)))
            .collect::<HashMap<_, _>>();

        for dpu_device_name in dpu_device_names {
            let cr_name = dpu_cr_name(dpu_device_name, dpf_id);
            let Some(dpu) = dpus_by_name.remove(&cr_name) else {
                continue;
            };
            let owner = dpu
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(DPU_OWNED_BY_DEPLOYMENT_LABEL));
            if owner == Some(&target_owner) {
                continue;
            }
            if owner != Some(&source_owner) {
                return Err(DpfError::InvalidState(format!(
                    "DPU {cr_name} is owned by neither DPUDeployment \
                     {source_deployment_name} nor {target_deployment_name}"
                )));
            }
            let uid = dpu.metadata.uid.as_deref().ok_or_else(|| {
                DpfError::InvalidState(format!(
                    "DPU {cr_name} has no UID for deployment migration deletion"
                ))
            })?;
            match DpuRepository::delete_if_uid(&*self.repo, &cr_name, &self.namespace, uid).await {
                Ok(()) => {}
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error),
            }
        }

        Ok(())
    }

    /// Find the one live DPUDeployment whose DPUSet selects a deployment type.
    async fn deployment_for_type(
        &self,
        deployment_type: DpuDeploymentType,
    ) -> Result<(String, DPUDeployment), DpfError> {
        let required_labels = self
            .labeler
            .node_labels_for_deployment_type(deployment_type)?;
        if required_labels.is_empty() {
            return Err(DpfError::ConfigError(format!(
                "DPUDeployment selector for {deployment_type:?} is empty"
            )));
        }

        let deployments = DpuDeploymentRepository::list(&*self.repo, &self.namespace).await?;
        let mut matching_deployments = deployments.into_iter().filter(|deployment| {
            deployment.metadata.deletion_timestamp.is_none()
                && dpu_deployment_selects_labels(deployment, &required_labels)
        });
        let deployment = matching_deployments.next().ok_or_else(|| {
            DpfError::InvalidState(format!(
                "no DPUDeployment selects {deployment_type:?} DPU nodes"
            ))
        })?;
        if matching_deployments.next().is_some() {
            return Err(DpfError::InvalidState(format!(
                "multiple DPUDeployments select {deployment_type:?} DPU nodes"
            )));
        }
        let deployment_name = deployment.metadata.name.clone().ok_or_else(|| {
            DpfError::InvalidState(format!(
                "DPUDeployment selecting {deployment_type:?} DPU nodes has no name"
            ))
        })?;

        Ok((deployment_name, deployment))
    }
}

/// Name of the singleton DPFOperatorConfig, as created by helm-prereqs and by the
/// manual install in `docs/manuals/dpf.md`.
const DPF_OPERATOR_CONFIG_NAME: &str = "dpfoperatorconfig";

impl<R: DpuDeploymentRepository + DpuRepository + DpfOperatorConfigRepository, L> DpfSdk<R, L> {
    /// Whether the DPF operator reports `Ready=True` at its current generation.
    ///
    /// Fails closed: absent, unreconciled, or condition-less all read as not
    /// ready, so callers that gate disruptive work skip rather than guess.
    async fn dpf_operator_config_is_ready(&self) -> Result<bool, DpfError> {
        let config = DpfOperatorConfigRepository::get(
            &*self.repo,
            DPF_OPERATOR_CONFIG_NAME,
            &self.namespace,
        )
        .await?;

        let Some(config) = config else {
            tracing::info!(
                name = DPF_OPERATOR_CONFIG_NAME,
                namespace = %self.namespace,
                "DPFOperatorConfig not found; treating DPF as not ready"
            );
            return Ok(false);
        };

        let ready = config
            .status
            .as_ref()
            .and_then(|status| status.conditions.as_ref())
            .and_then(|conditions| conditions.iter().find(|c| c.type_ == "Ready"))
            .is_some_and(|condition| {
                condition.status == "True"
                    && observed_generation_is_current(
                        condition.observed_generation,
                        config.metadata.generation,
                    )
            });

        if !ready {
            tracing::info!(
                name = DPF_OPERATOR_CONFIG_NAME,
                namespace = %self.namespace,
                "DPFOperatorConfig is not Ready; treating DPF as not ready"
            );
        }
        Ok(ready)
    }

    /// Find DPUs whose installed BFB, BlueFieldSoftware, or `spec.dpuFlavor` no
    /// longer matches the values declared on the DPUDeployment that owns them.
    ///
    /// Each DPU is expected to carry the
    /// `svc.dpu.nvidia.com/owned-by-dpudeployment` label (set by the DPF
    /// operator) whose value is `<namespace>_<deployment_name>`. We use that
    /// label to look up the owning DPUDeployment and read `spec.dpus.flavor`
    /// plus whichever provisioning source it declares — `spec.dpus.bfb` (BFB CR
    /// name) or `spec.dpus.blueFieldSoftware` (BlueFieldSoftware CR name) — for
    /// the comparison.
    ///
    /// Reading from the deployment — rather than from carbide config —
    /// keeps the comparison correct when multiple DPUDeployments coexist,
    /// each pinning their DPUs to a different image or flavor.
    ///
    /// The DPF operator stores the downloaded BFB on disk as
    /// `/bfb/<namespace>-<bfb_cr_name>.bfb` and reflects that path in
    /// `DPU.status.bfbFile`, so the expected filename is just
    /// `<namespace>-<spec.dpus.bfb>.bfb`. BlueFieldSoftware has no equivalent
    /// installed-version field in `DPU.status`, so it is compared against
    /// `DPU.spec.blueFieldSoftware`.
    ///
    /// DPUs are skipped (not flagged) when:
    /// - the owned-by label is missing or points to an unknown deployment,
    /// - the owning DPUDeployment is not currently reconciled
    ///   (`DPUSetsReconciled=True` with matching `observedGeneration`), or
    /// - the owning DPUDeployment declares neither or both provisioning
    ///   sources, which the DPU CRD forbids,
    ///
    /// to avoid acting on a partially-reconciled or mislabeled cluster.
    ///
    /// `dpu_label_selector` is forwarded to `DpuRepository::list` — pass the
    /// caller's controlled-device selector to limit the scan to its own DPUs.
    pub async fn find_outdated_dpus_dpf(
        &self,
        dpu_label_selector: Option<&str>,
    ) -> Result<Vec<DpuMismatch>, DpfError> {
        // A DPF upgrade republishes the CRs this scan reads, so mid-upgrade a DPU
        // can look outdated against a deployment that is still settling. Report
        // nothing until the operator says it is Ready, so an upgrade never
        // triggers reprovisioning on its own.
        if !self.dpf_operator_config_is_ready().await? {
            return Ok(vec![]);
        }

        let deployments = DpuDeploymentRepository::list(&*self.repo, &self.namespace).await?;
        let ready_deployments: HashMap<String, &DPUDeployment> = deployments
            .iter()
            .filter(|d| dpu_deployment_is_ready(d))
            .filter_map(|d| {
                let name = d.metadata.name.as_deref()?;
                Some((dpu_deployment_owner_label_value(&self.namespace, name), d))
            })
            .collect();

        if ready_deployments.is_empty() {
            tracing::debug!(
                namespace = %self.namespace,
                deployment_count = deployments.len(),
                "No DPUDeployment has DPUSetsReconciled=True with current observedGeneration; skipping DPF outdated scan"
            );
            return Ok(vec![]);
        }

        let dpus = DpuRepository::list(&*self.repo, &self.namespace, dpu_label_selector).await?;
        let mismatches = dpus
            .into_iter()
            .filter_map(|dpu| {
                let cr_name = dpu.metadata.name.clone()?;
                let owner_label = dpu
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|l| l.get(DPU_OWNED_BY_DEPLOYMENT_LABEL));
                let Some(owner_label) = owner_label else {
                    tracing::debug!(
                        dpu_name = %cr_name,
                        label = DPU_OWNED_BY_DEPLOYMENT_LABEL,
                        "DPU is missing label; skipping"
                    );
                    return None;
                };
                let Some(deployment) = ready_deployments.get(owner_label.as_str()) else {
                    tracing::debug!(
                        dpu_name = %cr_name,
                        owner = %owner_label,
                        "DPU's owning DPUDeployment is not ready or not found; skipping"
                    );
                    return None;
                };

                dpu_mismatch(&self.namespace, &dpu, deployment)
            })
            .collect();

        Ok(mismatches)
    }
}

/// Compare one DPU against the DPUDeployment that owns it.
///
/// Returns `None` when the DPU already matches the deployment (so a
/// reprovision would be pointless) and `Some` describing the drift when it does
/// not. Also returns `None` when the deployment is malformed, so a bad
/// deployment never triggers a fleet-wide reprovision.
///
/// Split out of [`DpfSdk::find_outdated_dpus_dpf`] so the comparison can be
/// exercised directly, without standing up repository mocks for a namespace
/// scan.
fn dpu_mismatch(namespace: &str, dpu: &DPU, deployment: &DPUDeployment) -> Option<DpuMismatch> {
    match dpu_comparison(namespace, dpu, deployment) {
        DpuComparison::Mismatch(mismatch) => Some(mismatch),
        DpuComparison::Match | DpuComparison::Inconclusive => None,
    }
}

/// The outcome of comparing one DPU against the DPUDeployment that owns it.
///
/// The third case is the point of this type: `Match` and `Inconclusive` are both
/// "no drift to report", but only one of them means the DPU is current. Callers
/// whose safe answer is to skip may treat them alike; callers that act on
/// "current" must not. Keeping them distinct in the return value is what stops a
/// future early return from silently reading as `Match`.
enum DpuComparison {
    /// The DPU matches everything its deployment declares.
    Match,
    /// The DPU differs from its deployment and needs reprovisioning.
    Mismatch(DpuMismatch),
    /// The pair could not be compared, so nothing is known either way.
    Inconclusive,
}

fn dpu_comparison(namespace: &str, dpu: &DPU, deployment: &DPUDeployment) -> DpuComparison {
    let Some(cr_name) = dpu.metadata.name.clone() else {
        return DpuComparison::Inconclusive;
    };
    // DPUFlavorTemplate is rendered into a per-DPU DPUFlavor by DPF, so the
    // template name cannot be compared with DPU.spec.dpuFlavor. Only ordinary
    // DPUFlavor deployments provide a flavor name that is meaningful here.
    let flavor_matches = deployment
        .spec
        .dpus
        .flavor
        .as_ref()
        .is_none_or(|expected_flavor| dpu.spec.dpu_flavor == *expected_flavor);

    // A DPUDeployment provisions from either a BFB or a BlueFieldSoftware CR;
    // the DPU CRD enforces that exactly one is set. BFB staleness is read from
    // `status.bfbFile`, the image actually installed. BlueFieldSoftware has no
    // installed-version field in status, so it is compared against `spec`
    // instead: the DPUSet strategy is OnDelete, so an existing DPU keeps the
    // spec it was created with, and a spec mismatch means it predates the
    // current deployment.
    let (source_matches, target_source) = match (
        deployment.spec.dpus.bfb.as_deref(),
        deployment.spec.dpus.blue_field_software.as_deref(),
    ) {
        (Some(expected_bfb_cr_name), None) => {
            let expected_filename = format!("{namespace}-{expected_bfb_cr_name}.bfb");
            let current_basename = dpu
                .status
                .as_ref()
                .and_then(|s| s.bfb_file.as_deref())
                .map(bfb_file_basename);
            let matches = current_basename == Some(expected_filename.as_str());
            (matches, expected_filename)
        }
        (None, Some(expected_software)) => {
            let matches = dpu.spec.blue_field_software.as_deref() == Some(expected_software);
            (matches, expected_software.to_string())
        }
        // Neither or both set violates the DPU CRD's
        // `has(self.bfb) != has(self.blueFieldSoftware)` rule. Skip rather than
        // reprovision every DPU off a malformed deployment.
        _ => {
            tracing::warn!(
                dpu_name = %cr_name,
                "Owning DPUDeployment sets neither or both of bfb and blueFieldSoftware; skipping"
            );
            return DpuComparison::Inconclusive;
        }
    };

    if source_matches && flavor_matches {
        return DpuComparison::Match;
    }
    DpuComparison::Mismatch(DpuMismatch {
        dpu_cr_name: cr_name,
        dpu_labels: dpu.metadata.labels.clone().unwrap_or_default(),
        target_source,
    })
}

/// Extract the trailing filename from a `DPU.status.bfbFile` path
/// (e.g. `/bfb/dpf-operator-system-bf-bundle-XXX.bfb` → `dpf-operator-system-bf-bundle-XXX.bfb`).
fn bfb_file_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Returns true when `metadata.generation` matches the
/// `DPUSetsReconciled` condition's `observedGeneration` and its status is `True`.
fn dpu_deployment_is_ready(d: &DPUDeployment) -> bool {
    let Some(generation) = d.metadata.generation else {
        return false;
    };
    let Some(status) = d.status.as_ref() else {
        return false;
    };
    let Some(conditions) = status.conditions.as_ref() else {
        return false;
    };
    let Some(cond) = conditions.iter().find(|c| c.type_ == "DPUSetsReconciled") else {
        return false;
    };
    cond.status == "True" && cond.observed_generation == Some(generation)
}

/// Returns true when one DPUSet in a deployment contains every required
/// DPUNode selector label.
fn dpu_deployment_selects_labels(
    deployment: &DPUDeployment,
    required_labels: &BTreeMap<String, String>,
) -> bool {
    deployment
        .spec
        .dpus
        .dpu_sets
        .as_ref()
        .is_some_and(|dpu_sets| {
            dpu_sets.iter().any(|dpu_set| {
                dpu_set
                    .dpu_node_selector
                    .as_ref()
                    .and_then(|selector| selector.match_labels.as_ref())
                    .is_some_and(|labels| {
                        required_labels
                            .iter()
                            .all(|(key, value)| labels.get(key) == Some(value))
                    })
            })
        })
}

/// True when a condition's `observedGeneration` matches the object's. Either
/// being absent means not ready: `metadata.generation` is set on submission, and
/// an absent `observedGeneration` means DPF has not reconciled the object yet.
fn observed_generation_is_current(observed: Option<i64>, generation: Option<i64>) -> bool {
    matches!((observed, generation), (Some(observed), Some(generation)) if observed == generation)
}

impl<R: DpuNodeMaintenanceRepository, L> DpfSdk<R, L> {
    /// Release the hold on a DPU node maintenance.
    /// If the DpuNodeMaintenance CR doesn't exist, this is a no-op
    /// (the hold is effectively already released).
    pub async fn release_maintenance_hold(&self, node_name: &str) -> Result<(), DpfError> {
        let maintenance_name = format!("{}-hold", node_name);
        let patch = json!({
            "metadata": {
                "annotations": {
                    HOLD_ANNOTATION: "false"
                }
            }
        });
        match DpuNodeMaintenanceRepository::patch(
            &*self.repo,
            &maintenance_name,
            &self.namespace,
            patch,
        )
        .await
        {
            Ok(()) => Ok(()),
            Err(DpfError::KubeError(kube::Error::Api(ref err))) if err.code == 404 => {
                tracing::debug!(
                    maintenance = %maintenance_name,
                    "DpuNodeMaintenance not found, hold already released"
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl<R: DpuRepository + DpuNodeRepository + DpuDeviceRepository, L: ResourceLabeler> DpfSdk<R, L> {
    /// Force delete a managed host and all its DPU resources.
    ///
    /// In the DPUDeployment (M4) model we remove the DPUNode and DPUDevices so DPF has no record
    /// of the DPU; no status patch to Error. Best-effort: remove controlled label, delete node,
    /// delete all DPU devices.
    ///
    /// `dpu_device_names` contains raw device IDs (without the `device-` CR prefix).
    pub async fn force_delete_host(
        &self,
        node_id: &str,
        dpu_device_names: &[String],
    ) -> Result<(), DpfError> {
        let node_name = &dpu_node_cr_name(node_id);
        let node = DpuNodeRepository::get(&*self.repo, node_name, &self.namespace).await?;

        if let Some(node) = node {
            let dpus = node.spec.dpus.unwrap_or_default();

            let patch = self.node_label_removal_patch();
            if let Err(e) =
                DpuNodeRepository::patch(&*self.repo, node_name, &self.namespace, patch).await
            {
                tracing::warn!(node_name, error = %e, "Failed to remove label from DPU node");
            }

            if let Err(e) = DpuNodeRepository::delete(&*self.repo, node_name, &self.namespace).await
            {
                tracing::warn!(node_name, error = %e, "Failed to delete DPU node");
            }

            // dpus[].name already has the device- prefix (set by register_dpu_node)
            for dpu in &dpus {
                if let Err(e) =
                    DpuDeviceRepository::delete(&*self.repo, &dpu.name, &self.namespace).await
                {
                    tracing::warn!(
                        dpu_device = %dpu.name,
                        error = %e,
                        "Failed to delete DPU device"
                    );
                }
            }
        } else {
            tracing::info!(
                node_name,
                "DPU node not found, trying to delete DPU devices"
            );
        }

        for name in dpu_device_names {
            let cr_name = dpu_device_cr_name(name);
            if let Err(e) =
                DpuDeviceRepository::delete(&*self.repo, &cr_name, &self.namespace).await
            {
                tracing::warn!(
                    dpu_device = %cr_name,
                    error = %e,
                    "Failed to delete DPU device"
                );
            }
        }

        Ok(())
    }

    /// Force delete a single DPU and its device.
    ///
    /// In M4 we delete the DPU CR and DPUDevice; no status patch to Error.
    /// `dpu_device_name` is the raw device ID (without the `device-` CR prefix).
    pub async fn force_delete_dpu(
        &self,
        dpu_device_name: &str,
        node_name: &str,
    ) -> Result<(), DpfError> {
        let dpf_id = node_id_from_dpu_node_cr_name(node_name);
        let cr_name = dpu_cr_name(dpu_device_name, dpf_id);
        if let Err(e) = DpuRepository::delete(&*self.repo, &cr_name, &self.namespace).await {
            tracing::warn!(dpu_name = %cr_name, error = %e, "Failed to delete DPU");
        }
        let device_cr_name = dpu_device_cr_name(dpu_device_name);
        if let Err(e) =
            DpuDeviceRepository::delete(&*self.repo, &device_cr_name, &self.namespace).await
        {
            tracing::warn!(
                dpu_device = %device_cr_name,
                error = %e,
                "Failed to delete DPU device"
            );
        }
        Ok(())
    }

    /// Force delete a DPU node and all its DPU devices.
    pub async fn force_delete_dpu_node(&self, node_name: &str) -> Result<(), DpfError> {
        let node = DpuNodeRepository::get(&*self.repo, node_name, &self.namespace).await?;
        let dpu_ids: Vec<String> = if let Some(ref n) = node {
            n.spec
                .dpus
                .as_ref()
                .map(|d| d.iter().map(|x| x.name.clone()).collect())
                .unwrap_or_default()
        } else {
            return Ok(());
        };
        let patch = self.node_label_removal_patch();
        if let Err(e) =
            DpuNodeRepository::patch(&*self.repo, node_name, &self.namespace, patch).await
        {
            tracing::warn!(node_name, error = %e, "Failed to remove label from DPU node");
        }
        if let Err(e) = DpuNodeRepository::delete(&*self.repo, node_name, &self.namespace).await {
            tracing::warn!(node_name, error = %e, "Failed to delete DPU node");
        }
        for dpu_id in &dpu_ids {
            if let Err(e) = DpuDeviceRepository::delete(&*self.repo, dpu_id, &self.namespace).await
            {
                tracing::warn!(
                    dpu_device = %dpu_id,
                    error = %e,
                    "Failed to delete DPU device"
                );
            }
        }
        Ok(())
    }
}

impl<R: DpuNodeRepository + DpuDeviceRepository + DpuRepository, L> DpfSdk<R, L> {
    /// Read a curated snapshot of the DPUNode, DPUDevices, and DPUs for a
    /// single host. `node_name` is the full `DPUNode` CR name (e.g.
    /// `node-<bmc-mac>`).
    ///
    /// Returns `dpu_node = None` when the DPUNode CR does not exist.
    /// Missing DPUDevice or DPU CRs (e.g. operator hasn't created the DPU
    /// yet) are silently skipped — the resulting snapshot reflects what's
    /// currently in K8s.
    pub async fn snapshot_host(&self, node_name: &str) -> Result<HostDpfSnapshot, DpfError> {
        let node = DpuNodeRepository::get(&*self.repo, node_name, &self.namespace).await?;

        let device_refs: Vec<String> = node
            .as_ref()
            .and_then(|n| n.spec.dpus.as_ref())
            .map(|dpus| dpus.iter().map(|d| d.name.clone()).collect())
            .unwrap_or_default();

        let dpu_node = node.as_ref().map(|n| DpuNodeSummary {
            name: n.metadata.name.clone().unwrap_or_default(),
            labels: n.metadata.labels.clone().unwrap_or_default(),
            annotations: n.metadata.annotations.clone().unwrap_or_default(),
            dpu_device_refs: device_refs.clone(),
        });

        let dpf_id = node_id_from_dpu_node_cr_name(node_name);

        let mut dpu_devices = Vec::with_capacity(device_refs.len());
        let mut dpus = Vec::with_capacity(device_refs.len());
        for device_ref in &device_refs {
            if let Some(dev) =
                DpuDeviceRepository::get(&*self.repo, device_ref, &self.namespace).await?
            {
                dpu_devices.push(DpuDeviceSummary {
                    name: dev.metadata.name.clone().unwrap_or_default(),
                    labels: dev.metadata.labels.clone().unwrap_or_default(),
                    bmc_ip: dev.spec.bmc_ip.clone(),
                    bmc_port: dev.spec.bmc_port,
                    serial_number: dev.spec.serial_number.clone(),
                });
            }

            // device_ref on DPUNode.spec.dpus has the `device-` prefix the
            // operator uses; strip it to recover the raw device_id needed by
            // dpu_cr_name().
            let raw_device_id = device_ref
                .strip_prefix("device-")
                .unwrap_or(device_ref.as_str());
            let dpu_cr = dpu_cr_name(raw_device_id, dpf_id);
            if let Some(d) = DpuRepository::get(&*self.repo, &dpu_cr, &self.namespace).await? {
                dpus.push(DpuSummary {
                    name: d.metadata.name.clone().unwrap_or_default(),
                    labels: d.metadata.labels.clone().unwrap_or_default(),
                    spec_bfb: d.spec.bfb.clone().unwrap_or_default(),
                    spec_dpu_flavor: Some(d.spec.dpu_flavor.clone()),
                    spec_dpu_device_name: d.spec.dpu_device_name.clone(),
                    spec_dpu_node_name: d.spec.dpu_node_name.clone(),
                    status_phase: d.status.as_ref().map(|s| format!("{:?}", s.phase)),
                    status_bfb_file: d.status.as_ref().and_then(|s| s.bfb_file.clone()),
                    status_conditions: d.status.as_ref().and_then(|s| s.conditions.clone()),
                    status_operational_conditions: d
                        .status
                        .as_ref()
                        .and_then(|s| s.operational_conditions.clone()),
                    status_agent_status: d.status.as_ref().and_then(|s| s.agent_status.clone()),
                });
            }
        }

        Ok(HostDpfSnapshot {
            dpu_node,
            dpu_devices,
            dpus,
        })
    }
}

impl<R: DpuServiceTemplateRepository, L> DpfSdk<R, L> {
    /// List the helm-chart versions currently declared on each live
    /// `DPUServiceTemplate` CR. Useful for comparing what's deployed in
    /// the cluster against the carbide-config service versions.
    pub async fn list_service_template_versions(
        &self,
    ) -> Result<Vec<ServiceTemplateVersion>, DpfError> {
        let templates = DpuServiceTemplateRepository::list(&*self.repo, &self.namespace).await?;
        Ok(templates
            .into_iter()
            .map(|t| {
                let docker_image_tag = t
                    .spec
                    .helm_chart
                    .values
                    .as_ref()
                    .and_then(|v| v.get("image"))
                    .and_then(|img| img.get("tag"))
                    .and_then(|tag| tag.as_str())
                    .unwrap_or_default()
                    .to_string();
                ServiceTemplateVersion {
                    cr_name: t.metadata.name.unwrap_or_default(),
                    deployment_service_name: t.spec.deployment_service_name,
                    helm_repo_url: t.spec.helm_chart.source.repo_url,
                    helm_chart: t.spec.helm_chart.source.chart,
                    helm_version: t.spec.helm_chart.source.version,
                    docker_image_tag,
                }
            })
            .collect())
    }
}

impl<R: DpuRepository + DpuDeploymentRepository + DpuServiceTemplateRepository, L> DpfSdk<R, L> {
    /// Fetch a DPU CR together with the DPUDeployment that owns it, resolved
    /// through the `svc.dpu.nvidia.com/owned-by-dpudeployment` label.
    ///
    /// Returns the deployment's name alongside it, since callers report it in
    /// errors and log lines.
    async fn dpu_with_owning_deployment(
        &self,
        dpu_name: &str,
    ) -> Result<(DPU, String, DPUDeployment), DpfError> {
        let dpu = DpuRepository::get(&*self.repo, dpu_name, &self.namespace)
            .await?
            .ok_or_else(|| DpfError::InvalidState(format!("DPU CR not found: {dpu_name}")))?;

        let owner_label = dpu
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get(DPU_OWNED_BY_DEPLOYMENT_LABEL))
            .ok_or_else(|| {
                DpfError::InvalidState(format!(
                    "DPU {dpu_name} is missing {DPU_OWNED_BY_DEPLOYMENT_LABEL} label"
                ))
            })?;

        let deployment_name = owner_label
            .strip_prefix(&format!("{}_", self.namespace))
            .unwrap_or(owner_label.as_str())
            .to_string();

        let deployment =
            DpuDeploymentRepository::get(&*self.repo, &deployment_name, &self.namespace)
                .await?
                .ok_or_else(|| {
                    DpfError::InvalidState(format!(
                        "DPUDeployment {deployment_name} not found for DPU {dpu_name}"
                    ))
                })?;

        Ok((dpu, deployment_name, deployment))
    }

    /// Whether one DPU's installed BFB, BlueFieldSoftware, or flavor differs
    /// from what its owning DPUDeployment declares.
    ///
    /// This answers "is a reprovision still coming for this DPU", which gates
    /// work that must not land on an OS about to be replaced.
    ///
    /// Note the deliberate asymmetry with [`Self::find_outdated_dpus_dpf`],
    /// which skips a DPU it cannot evaluate so a malformed deployment never
    /// triggers a fleet-wide reprovision. Here an unevaluable DPU reports
    /// `true`: the caller's safe action is to do nothing, so "unknown" must not
    /// be reported as "up to date".
    pub async fn is_dpu_outdated(&self, dpu_name: &str) -> Result<bool, DpfError> {
        let (dpu, deployment_name, deployment) = self.dpu_with_owning_deployment(dpu_name).await?;

        if !dpu_deployment_is_ready(&deployment) {
            tracing::info!(
                dpu_name,
                deployment = %deployment_name,
                "DPU's owning DPUDeployment is not ready; treating the DPU as outdated"
            );
            return Ok(true);
        }

        match dpu_comparison(&self.namespace, &dpu, &deployment) {
            DpuComparison::Match => Ok(false),
            DpuComparison::Mismatch(mismatch) => {
                tracing::info!(
                    dpu_name,
                    deployment = %deployment_name,
                    target_source = %mismatch.target_source,
                    "DPU does not match its owning DPUDeployment"
                );
                Ok(true)
            }
            DpuComparison::Inconclusive => {
                tracing::warn!(
                    dpu_name,
                    deployment = %deployment_name,
                    "DPU could not be compared against its owning DPUDeployment; \
                     treating it as outdated"
                );
                Ok(true)
            }
        }
    }

    /// Resolve the installed service versions for a DPU by looking up its owning
    /// DPUDeployment (via the `svc.dpu.nvidia.com/owned-by-dpudeployment` label on the DPU CR)
    /// and reading each service's DPUServiceTemplate.
    ///
    /// Each returned [`DpuServiceVersion`] is derived per field:
    /// - `version`: `helmChart.values.image.tag` when set and non-empty, else
    ///   `helmChart.source.version`.
    /// - `url` + `name`: when `helmChart.values.image.repository` is set, it is
    ///   split at its final `/` into `url` (registry/path) and `name` (image name);
    ///   otherwise `url` is `helmChart.source.repoURL` and `name` is
    ///   `helmChart.source.chart`. If no name can be derived, the DPUDeployment
    ///   service name is used.
    ///
    /// Returns an error when any referenced DPUServiceTemplate is absent so
    /// callers cannot persist a partial inventory snapshot.
    pub async fn get_service_versions_for_dpu(
        &self,
        dpu_name: &str,
    ) -> Result<Vec<DpuServiceVersion>, DpfError> {
        let (_dpu, deployment_name, deployment) = self.dpu_with_owning_deployment(dpu_name).await?;

        let mut versions = Vec::new();
        for (service_name, service) in &deployment.spec.services {
            let Some(template_name) = &service.service_template else {
                continue;
            };
            let template =
                DpuServiceTemplateRepository::get(&*self.repo, template_name, &self.namespace)
                    .await?
                    .ok_or_else(|| {
                        DpfError::InvalidState(format!(
                            "DPUServiceTemplate {template_name} not found for service \
                             {service_name} in DPUDeployment {deployment_name}"
                        ))
                    })?;

            let image_values = template
                .spec
                .helm_chart
                .values
                .as_ref()
                .and_then(|v| v.get("image"));
            let image_tag = image_values
                .and_then(|img| img.get("tag"))
                .and_then(|tag| tag.as_str())
                .filter(|s| !s.is_empty());
            let image_repo = image_values
                .and_then(|img| img.get("repository"))
                .and_then(|r| r.as_str())
                .filter(|s| !s.is_empty());

            // Version is the image tag when set, otherwise the Helm chart version.
            // These are independent of the image repository: a template that only
            // overrides the tag must still report that tag.
            let version = image_tag
                .map(str::to_string)
                .unwrap_or_else(|| template.spec.helm_chart.source.version.clone());

            // url + name: split the image repository at its final '/' when present
            // (registry/path as url, image name as name); otherwise fall back to the
            // Helm source repo URL and chart name.
            let (url, mut name) = if let Some(repo) = image_repo {
                repo.rsplit_once('/')
                    .map(|(prefix, base)| (prefix.to_string(), base.to_string()))
                    .unwrap_or_else(|| (String::new(), repo.to_string()))
            } else {
                (
                    template.spec.helm_chart.source.repo_url.clone(),
                    template
                        .spec
                        .helm_chart
                        .source
                        .chart
                        .clone()
                        .unwrap_or_default(),
                )
            };

            // Never emit a nameless component; the DPUDeployment service name is a
            // stable identifier when neither the image basename nor chart name is set.
            if name.is_empty() {
                name = service_name.clone();
            }

            versions.push(DpuServiceVersion { name, version, url });
        }

        Ok(versions)
    }
}

impl<R: DpuRepository, L: ResourceLabeler> DpfSdk<R, L> {
    /// Create a watcher builder for DPF events.
    ///
    /// The watcher monitors DPU resources and invokes
    /// callbacks when:
    /// - A DPU's phase changes
    /// - A host reboot is required
    /// - A DPU becomes ready
    /// - Maintenance is needed for a node
    ///
    /// The watcher uses repository traits for all IO, making it testable
    /// with mock repositories.
    ///
    /// Call `.start()` on the returned builder to begin watching.
    pub fn watcher(&self) -> DpuWatcherBuilder<'_, R> {
        let mut builder = DpuWatcherBuilder::new(self.repo.clone(), self.namespace.clone());
        if let Some(selector) = self.labeler.dpu_label_selector() {
            builder = builder.with_label_selector(selector);
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::future::Future;
    use std::sync::{Arc, RwLock};

    use async_trait::async_trait;
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{scenarios, value_scenarios};
    use kube::Resource;

    use super::*;
    use crate::crds::dpuflavors_generated::DPUFlavor;
    use crate::crds::dpuflavortemplates_generated::DPUFlavorTemplate;
    use crate::crds::dpus_generated::{DPU, DpuNodeEffect};
    use crate::crds::dpuservices_generated::DPUService;
    use crate::repository::{
        DpuDeviceRepository, DpuFlavorRepository, DpuFlavorTemplateRepository, DpuNodeRepository,
        DpuRepository, DpuServiceRepository,
    };
    use crate::types::{
        DetachedHelmChart, DpfInterceptBridge, DpfInterceptBridging, DpfInterfaceIdentity,
        DpfProxyDetails, DpuDeviceInfo, DpuNodeInfo,
    };

    /// Verifies scoped ServiceInterface names distinguish every deployment class.
    #[test]
    fn service_interface_suffixes_cover_all_deployment_types() {
        value_scenarios!(
            run = service_interface_cr_suffix;
            "BF3" {
                // BF3 is suffixed too because scoped mode is an explicit namespace-wide migration.
                DpuDeploymentType::Bf3 => "bf3",
            }

            "GB200 BF3" {
                // The specialized flavor needs its own selector and interface resources.
                DpuDeploymentType::Bf3Gb200 => "bf3gb200",
            }

            "generic BF4" {
                // Generic BF4 must not share interface resources with BF3 or Astra.
                DpuDeploymentType::Bf4Generic => "bf4",
            }

            "BF4 Astra" {
                // Astra's BF4+CX9 inventory remains isolated from generic BF4.
                DpuDeploymentType::Bf4Astra => "astra",
            }
        );
    }

    /// Verifies controller selectors are confined to the explicit deployment-scoping migration.
    #[test]
    fn pf_and_vf_nic_selectors_follow_interface_scope() {
        let interfaces = build_dpu_interfaces_vec();
        let dpu_cluster_node_labels =
            BTreeMap::from([("deployment".to_string(), "bf3".to_string())]);
        let controller_number = |interface: &DPUServiceInterface| {
            let spec = &interface.spec.template.spec.template.spec;
            spec.pf
                .as_ref()
                .and_then(|pf| pf.nic_selector.as_ref())
                .and_then(|selector| selector.controller_number)
                .or_else(|| {
                    spec.vf
                        .as_ref()
                        .and_then(|vf| vf.nic_selector.as_ref())
                        .and_then(|selector| selector.controller_number)
                })
        };

        for interface_name in ["pf0hpf", "pf0vf0"] {
            let definition = interfaces
                .iter()
                .find(|interface| interface.name == interface_name)
                .expect("static PF/VF definition must exist");

            // The public legacy builder must retain its byte-compatible absent selector.
            assert_eq!(
                controller_number(&build_service_interface(definition, TEST_NAMESPACE)),
                None,
            );

            // Scoped resources explicitly select DPU controller 1.
            assert_eq!(
                controller_number(&build_service_interface_with_scope(
                    definition,
                    TEST_NAMESPACE,
                    "bf3",
                    Some(&dpu_cluster_node_labels),
                )),
                Some(1),
            );
        }
    }

    /// Counts effective VFs and per-service endpoints in one interface inventory.
    fn interface_counts(
        interfaces: &[DpuServiceInterfaceTemplateDefinition],
    ) -> (usize, usize, usize, usize, usize) {
        let endpoint_count = |service_name: &str| {
            interfaces
                .iter()
                .filter(|interface| {
                    interface.chained_svc_if.as_ref().is_some_and(|chains| {
                        chains.iter().any(|(service, _)| service == service_name)
                    })
                })
                .count()
        };
        (
            interfaces
                .iter()
                .filter(|interface| {
                    matches!(&interface.iface_type, DpuServiceInterfaceTemplateType::Vf)
                })
                .count(),
            interfaces.len(),
            endpoint_count(DOCA_HBN_SERVICE_NAME),
            endpoint_count(DHCP_SERVER_SERVICE_NAME),
            endpoint_count(FMDS_SERVICE_NAME),
        )
    }

    /// Provides a validated configured topology for effective-inventory tests.
    fn configured_topology() -> DpfInterceptBridging {
        DpfInterceptBridging::new(
            vec![
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: Some(4),
                    },
                    "br-vf4",
                    "p-vf4",
                ),
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: None,
                    },
                    "br-pf3",
                    "p-pf3",
                ),
            ],
            16,
        )
        .expect("configured inventory fixture must be valid")
    }

    /// Provides BF3 initialization inputs for flavor persistence and conflict tests.
    fn flavor_test_config(proxy: Option<DpfProxyDetails>) -> InitDpfResourcesConfig {
        InitDpfResourcesConfig {
            proxy,
            ..Default::default()
        }
    }

    /// Verifies static inventory filtering pins minimum, default, and maximum counts.
    #[test]
    fn effective_static_inventory_follows_provisioned_vf_count() {
        value_scenarios!(
            run = |num_of_vfs| interface_counts(&build_effective_dpu_interfaces(num_of_vfs, None));
            "no provisioned VFs" {
                // Fixed physical and PF entries remain when hardware exposes no VFs.
                0 => (0, 4, 4, 1, 1),
            }

            "default VF population" {
                // Retain pf0vf0 through pf0vf13, including the legacy policy where only VF0–VF7
                // receive DHCP endpoints and VF8–VF13 remain HBN-only.
                16 => (14, 18, 18, 9, 1),
            }

            "maximum VF population" {
                // Hardware VFs above pf0vf13 remain intentionally unclaimed in static mode.
                126 => (14, 18, 18, 9, 1),
            }
        );
    }

    /// Verifies configured intercept bridging is the complete PF/VF inventory and service policy.
    #[test]
    fn effective_configured_inventory_replaces_static_pf_and_vf_entries() {
        // Build one configured PF and one configured VF outside the historical static identities.
        let topology = configured_topology();
        let interfaces = build_effective_dpu_interfaces(16, Some(&topology));

        // Only p0, p1, and the two configured Patch interfaces remain.
        assert_eq!(interface_counts(&interfaces), (0, 4, 4, 2, 1));
        assert_eq!(
            interfaces
                .iter()
                .map(|interface| interface.name.as_str())
                .collect::<Vec<_>>(),
            ["p0", "p1", "c2pf3", "c2pf3vf4"]
        );
        assert!(interfaces[2..].iter().all(|interface| matches!(
            &interface.iface_type,
            DpuServiceInterfaceTemplateType::Patch(_)
        )));

        // Each generated DPUDeployment service-chain switch must select the same logical label as
        // its ServiceInterface; otherwise the chain cannot bind to that interface.
        let services = [
            DOCA_HBN_SERVICE_NAME,
            DHCP_SERVER_SERVICE_NAME,
            FMDS_SERVICE_NAME,
        ]
        .into_iter()
        .map(|name| ServiceDefinition::new(name, "repo", "chart", "1"))
        .collect::<Vec<_>>();
        let deployment = build_deployment(
            &services,
            "deployment",
            &DpuProvisioningSource::Bfb("bfb".to_string()),
            "flavor",
            TEST_NAMESPACE,
            &interfaces,
            BTreeMap::new(),
            DpuDeploymentType::Bf3,
        );
        let switches = deployment.spec.service_chains.unwrap().switches;
        assert_eq!(switches.len(), interfaces.len());
        assert_eq!(
            switches
                .iter()
                .map(|switch| {
                    switch.ports[0]
                        .service_interface
                        .as_ref()
                        .unwrap()
                        .match_labels["interface"]
                        .as_str()
                })
                .collect::<Vec<_>>(),
            ["p0", "p1", "c2pf3", "c2pf3vf4"]
        );
    }

    /// Verifies configured inventories add their exact service endpoint population to the reserved
    /// SF capacity while inventory-free deployments retain their legacy total.
    #[test]
    fn pf_total_sf_follows_effective_inventory_and_compatibility_mode() {
        // Build both meaningful inventory modes from the same production projection path.
        let static_interfaces = build_effective_dpu_interfaces(16, None);
        let configured_topology = DpfInterceptBridging::new(
            std::iter::once(DpfInterceptBridge::new(
                DpfInterfaceIdentity {
                    controller_id: 2,
                    pf_id: 3,
                    vf_id: None,
                },
                "br-pf3",
                "p-pf3",
            ))
            .chain((0..=15).map(|vf_id| {
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: Some(vf_id),
                    },
                    format!("br-vf{vf_id}"),
                    format!("p-vf{vf_id}"),
                )
            }))
            .collect(),
            16,
        )
        .expect("one PF and sixteen VFs must be a valid configured topology");
        let configured_interfaces = build_effective_dpu_interfaces(16, Some(&configured_topology));

        // The configured cases count every HBN, DHCP, and FMDS endpoint, including fixed uplinks.
        value_scenarios!(
            run = |(interfaces, intercept_bridging, additional_managed_sf)| {
                calculate_pf_total_sf(
                    interfaces,
                    intercept_bridging,
                    DEFAULT_PF_TOTAL_SF_RESERVED,
                    additional_managed_sf,
                )
                .unwrap()
            };
            "legacy static inventory" {
                // Static endpoints are intentionally not added because doing so would reprovision existing DPUs.
                (&static_interfaces, None, 0) => 30,
            }

            "legacy static inventory does not change for an additional managed SF" {
                (&static_interfaces, None, 1) => 30,
            }

            "configured PF and sixteen VF inventory" {
                // Two fixed HBN, three PF, and two endpoints per VF consume 37 managed SFs.
                (&configured_interfaces, Some(&configured_topology), 0) => 67,
            }

            "configured inventory with an additional managed SF" {
                (&configured_interfaces, Some(&configured_topology), 1) => 68,
            }
        );

        // The maximum supported topology remains below HBN's 32-interface boundary.
        assert_eq!(interface_counts(&configured_interfaces), (0, 19, 19, 17, 1));
    }

    /// Verifies legacy managed endpoints cannot overcommit the unchanged SF pool.
    #[test]
    fn legacy_pf_total_sf_rejects_endpoint_overcommit() {
        let interfaces = build_effective_dpu_interfaces(16, None);

        scenarios!(
            run = |additional_managed_sf: u32| {
                calculate_pf_total_sf(
                    &interfaces,
                    None,
                    DEFAULT_PF_TOTAL_SF_RESERVED,
                    additional_managed_sf,
                )
                .map_err(drop)
            };

            "generated and additional endpoints fit" {
                2 => Yields(DEFAULT_PF_TOTAL_SF_RESERVED),
            }

            "additional endpoints overcommit the pool" {
                3 => Fails,
            }
        );
    }

    /// Verifies invalid SF arithmetic is rejected before it can become a wrapped NVConfig value.
    #[test]
    fn pf_total_sf_rejects_overflow() {
        // Any configured endpoint added to the maximum reserve must overflow.
        let topology = configured_topology();
        let interfaces = build_effective_dpu_interfaces(16, Some(&topology));

        // Configuration failure is preferable to emitting an unusable DPUFlavor.
        assert!(matches!(
            calculate_pf_total_sf(&interfaces, Some(&topology), u32::MAX, 0),
            Err(DpfError::ConfigError(_))
        ));
    }

    /// Verifies custom SDK inventories cannot exceed HBN's interface capacity.
    #[test]
    fn topology_rejects_more_than_thirty_two_hbn_interfaces() {
        // A valid topology selects endpoint-derived sizing; custom callers may supply their own
        // rendered interface vector, so the guard must validate that vector directly.
        let topology = configured_topology();
        let interfaces = vec![DpuServiceInterfaceTemplateDefinition {
            name: "oversized-hbn".to_string(),
            iface_type: DpuServiceInterfaceTemplateType::Physical,
            pf_id: 0,
            vf_id: 0,
            chained_svc_if: Some(
                (0..=MAX_HBN_SERVICE_INTERFACES)
                    .map(|index| (DOCA_HBN_SERVICE_NAME.to_string(), format!("hbn{index}_if")))
                    .collect(),
            ),
        }];

        // Rejecting during pure capacity resolution keeps the invalid inventory out of Kubernetes.
        assert!(matches!(
            calculate_pf_total_sf(
                &interfaces,
                Some(&topology),
                DEFAULT_PF_TOTAL_SF_RESERVED,
                0,
            ),
            Err(DpfError::ConfigError(message)) if message.contains("exceeding the supported maximum of 32")
        ));
    }

    /// Verifies a topology cannot be initialized with a different VF population than the one
    /// against which its identities and provisioned representor names were validated.
    #[test]
    fn initialization_rejects_mismatched_topology_vf_count() {
        // The shared configured topology is validated for the default population of 16 VFs.
        let config = InitDpfResourcesConfig {
            num_of_vfs: 8,
            intercept_bridging: Some(configured_topology()),
            ..Default::default()
        };

        // Pure preflight must reject the mismatch before any repository write is possible.
        assert!(matches!(
            resolve_initialization_inventory(&config),
            Err(DpfError::ConfigError(message))
                if message.contains("validated for num_of_vfs=16")
                    && message.contains("requested num_of_vfs=8")
        ));
    }

    /// Verifies a caller-provided inventory cannot diverge from its authoritative topology.
    #[test]
    fn initialization_rejects_mismatched_topology_interface_projection() {
        let topology = configured_topology();
        let mut interfaces =
            build_effective_dpu_interfaces(crate::DEFAULT_DPU_NUM_OF_VFS, Some(&topology));
        interfaces[1].name = "unexpected".to_string();
        let config = InitDpfResourcesConfig {
            intercept_bridging: Some(topology),
            // A non-empty custom inventory previously bypassed projection and could diverge from
            // the Patch-backed HBN endpoints used to generate the DHCP ACL.
            interfaces,
            ..Default::default()
        };

        assert!(matches!(
            resolve_initialization_inventory(&config),
            Err(DpfError::ConfigError(message))
                if message.contains("first differing interface: received unexpected, expected p1")
                    && message.contains("received 4 interfaces, expected 4")
        ));
    }

    /// Verifies callers may still provide the canonical topology projection explicitly.
    #[test]
    fn initialization_accepts_matching_topology_interface_projection() {
        let topology = configured_topology();
        let interfaces =
            build_effective_dpu_interfaces(crate::DEFAULT_DPU_NUM_OF_VFS, Some(&topology));
        let config = InitDpfResourcesConfig {
            intercept_bridging: Some(topology),
            interfaces: interfaces.clone(),
            ..Default::default()
        };

        let resolved = resolve_initialization_inventory(&config)
            .expect("canonical custom topology projection must be accepted");
        assert_eq!(resolved.interfaces.as_ref(), interfaces.as_slice());
    }

    /// Verifies explicit Astra inventories are augmented without changing non-Astra callers.
    #[test]
    fn initialization_augments_explicit_astra_interface_projection_only() {
        let base_interfaces = build_dpu_interfaces_vec();
        let astra_config = InitDpfResourcesConfig {
            deployment_scoped_service_interfaces: true,
            deployment_type: DpuDeploymentType::Bf4Astra,
            interfaces: base_interfaces.clone(),
            ..Default::default()
        };

        let astra_resolved = resolve_initialization_inventory(&astra_config)
            .expect("explicit Astra inventory must be accepted");
        assert_eq!(
            &astra_resolved.interfaces.as_ref()[..base_interfaces.len()],
            base_interfaces.as_slice()
        );
        assert_eq!(
            astra_resolved.interfaces.len(),
            base_interfaces.len() + build_astra_patch_dpu_interfaces_vec().len()
        );
        assert!(
            astra_resolved
                .interfaces
                .iter()
                .any(|interface| interface.name == "p-brcx-r0swpln0-to-br-sfc")
        );
        assert!(
            astra_resolved
                .interfaces
                .iter()
                .any(|interface| interface.name == "p-br-xplane-r3swpln1-to-br-sfc")
        );

        let bf3_config = InitDpfResourcesConfig {
            interfaces: base_interfaces.clone(),
            ..Default::default()
        };
        let bf3_resolved = resolve_initialization_inventory(&bf3_config)
            .expect("explicit BF3 inventory must be accepted unchanged");
        assert_eq!(bf3_resolved.interfaces.as_ref(), base_interfaces.as_slice());
    }

    /// Astra's NICo-owned patch names cannot be rebound by a direct SDK caller.
    #[test]
    fn initialization_rejects_conflicting_astra_patch_interface() {
        let mut interfaces = build_dpu_interfaces_vec();
        interfaces.push(DpuServiceInterfaceTemplateDefinition {
            name: "p-brcx-r0swpln0-to-br-sfc".to_string(),
            iface_type: DpuServiceInterfaceTemplateType::Physical,
            pf_id: 0,
            vf_id: 0,
            chained_svc_if: None,
        });
        let config = InitDpfResourcesConfig {
            deployment_scoped_service_interfaces: true,
            deployment_type: DpuDeploymentType::Bf4Astra,
            interfaces,
            ..Default::default()
        };

        assert!(matches!(
            resolve_initialization_inventory(&config),
            Err(DpfError::ConfigError(message))
                if message.contains("p-brcx-r0swpln0-to-br-sfc")
                    && message.contains("reserved")
        ));
    }

    /// Astra capacity follows its managed endpoints, the Weave DHCP Agent allocation, and fixed
    /// headroom; the BF3/generic reserve must not change the Astra flavor.
    #[test]
    fn astra_pf_total_sf_ignores_site_reserve() {
        let config = InitDpfResourcesConfig {
            deployment_scoped_service_interfaces: true,
            deployment_type: DpuDeploymentType::Bf4Astra,
            // A value that would overflow if Astra incorrectly treated this as additional SF
            // capacity proves the reserve is not part of Astra's calculation.
            pf_total_sf_reserved: u32::MAX,
            ..Default::default()
        };

        let resolved = resolve_initialization_inventory(&config)
            .expect("Astra capacity must not consume the BF3/generic SF reserve");
        let astra_interfaces = build_astra_dpu_interfaces_vec();
        let managed_endpoints = astra_interfaces
            .iter()
            .map(|interface| interface.chained_svc_if.as_ref().map_or(0, Vec::len) as u32)
            .sum::<u32>();
        assert_eq!(
            resolved.pf_total_sf,
            managed_endpoints + DOCA_WEAVE_DHCP_AGENT_PF_TOTAL_SF + PF_TOTAL_SF_BF4_ASTRA_FUDGE
        );
    }

    /// Verifies the public initialization boundary rejects unsupported hardware VF populations.
    #[test]
    fn initialization_rejects_hardware_vf_count_above_platform_limit() {
        // Direct SDK callers do not pass through api-core configuration deserialization.
        let config = InitDpfResourcesConfig {
            num_of_vfs: MAX_BLUEFIELD_VFS_PER_PF + 1,
            ..Default::default()
        };

        // Pure preflight runs before the SDK writes its shared BMC Secret.
        assert!(matches!(
            resolve_initialization_inventory(&config),
            Err(DpfError::ConfigError(message)) if message.contains("num_of_vfs must be <= 126")
        ));
    }

    /// Verifies Patch CR serialization follows the installed controller contract.
    #[test]
    fn configured_interface_serializes_one_dpf_owned_patch() {
        // Select the configured PF from the shared effective inventory.
        let topology = configured_topology();
        let interfaces = build_effective_dpu_interfaces(16, Some(&topology));
        let configured_pf = interfaces
            .iter()
            .find(|interface| interface.name == "c2pf3")
            .expect("configured PF interface must exist");

        // Patch serialization owns only the peer pair and omits optional caller metadata such as
        // `peerExternalIDs`.
        let cr = build_service_interface(configured_pf, TEST_NAMESPACE);
        let spec = &cr.spec.template.spec.template.spec;
        let patch = spec.patch.as_ref().expect("Patch definition must be set");
        assert_eq!(patch.peer_bridge, "br-pf3");
        assert_eq!(patch.peer_patch_name.as_deref(), Some("p-pf3"));
        assert!(patch.peer_external_i_ds.is_none());
        assert!(spec.pf.is_none() && spec.vf.is_none() && spec.physical.is_none());
    }

    /// Verifies Astra's static CX patch interfaces serialize the installed controller contract.
    #[test]
    fn astra_inventory_serializes_cx_and_xplane_patches() {
        let interfaces = build_astra_dpu_interfaces_vec();
        let by_name = |name: &str| {
            interfaces
                .iter()
                .find(|interface| interface.name == name)
                .unwrap_or_else(|| panic!("Astra interface {name} must exist"))
        };

        let cx = build_service_interface(by_name("p-brcx-r0swpln0-to-br-sfc"), TEST_NAMESPACE);
        let cx_patch = cx.spec.template.spec.template.spec.patch.as_ref().unwrap();
        assert_eq!(cx_patch.peer_bridge, "brcx-r0swpln0");
        assert!(cx_patch.peer_patch_name.is_none());
        assert!(cx_patch.peer_external_i_ds.is_none());

        let xplane =
            build_service_interface(by_name("p-br-xplane-r3swpln1-to-br-sfc"), TEST_NAMESPACE);
        let xplane_patch = xplane
            .spec
            .template
            .spec
            .template
            .spec
            .patch
            .as_ref()
            .unwrap();
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
    }

    fn already_exists_error(name: &str) -> DpfError {
        DpfError::KubeError(kube::Error::Api(Box::new(
            kube::core::Status::failure(&format!("{name} already exists"), "AlreadyExists")
                .with_code(409),
        )))
    }

    /// Builds the Kubernetes response returned for an already absent resource.
    fn not_found_error(name: &str) -> DpfError {
        DpfError::KubeError(kube::Error::Api(Box::new(
            kube::core::Status::failure(&format!("{name} was not found"), "NotFound")
                .with_code(404),
        )))
    }

    const TEST_NAMESPACE: &str = "test-namespace";

    #[test]
    fn otelcol_depends_on_includes_dts() {
        // Regression: otelcol templates its DTS scrape target from
        // `{{ (index .Services "dts").Name }}`. That lookup only resolves when
        // dts is declared as a dependency of the otelcol service, otherwise the
        // rendered target host is `carbide-dpf-cluster--doca-telemetry` (empty
        // name) and DTS metrics never get scraped.
        let svc = |name: &str| ServiceDefinition::new(name, "repo", "chart", "1.0.0");
        let services = vec![
            svc(OTEL_COLLECTOR_SERVICE_NAME),
            svc(DTS_SERVICE_NAME),
            svc(DPU_AGENT_SERVICE_NAME),
            svc(FMDS_SERVICE_NAME),
        ];

        let deployment = build_deployment(
            &services,
            "dep",
            &DpuProvisioningSource::Bfb("bfb".to_string()),
            "flavor",
            TEST_NAMESPACE,
            &[],
            BTreeMap::new(),
            DpuDeploymentType::Bf3,
        );

        let otel = deployment
            .spec
            .services
            .get(OTEL_COLLECTOR_SERVICE_NAME)
            .expect("otelcol service present");
        let deps: Vec<String> = otel
            .depends_on
            .as_ref()
            .expect("otelcol should declare dependencies")
            .iter()
            .map(|d| d.name.clone())
            .collect();

        assert!(
            deps.contains(&DTS_SERVICE_NAME.to_string()),
            "otelcol must depend on dts so its scrape target resolves; got {deps:?}"
        );
        // The previously-working dependencies must remain.
        assert!(deps.contains(&DPU_AGENT_SERVICE_NAME.to_string()));
        assert!(deps.contains(&FMDS_SERVICE_NAME.to_string()));
    }

    /// Service/NAD CR names are suffixed per deployment so BF3 and BF4 don't
    /// overwrite each other, but BF3 keeps its original (unsuffixed) names so
    /// existing clusters are untouched. The logical `deploymentServiceName` and
    /// the DPUDeployment `services` map keys are never suffixed.
    #[test]
    fn service_cr_names_suffix_only_non_bf3() {
        let svc = ServiceDefinition::new(DOCA_HBN_SERVICE_NAME, "repo", "chart", "1.0.5");

        let bf3_suffix = deployment_cr_suffix(DpuDeploymentType::Bf3);
        let bf3_gb200_suffix = deployment_cr_suffix(DpuDeploymentType::Bf3Gb200);
        let bf4_suffix = deployment_cr_suffix(DpuDeploymentType::Bf4Generic);

        // BF3: CR name unchanged.
        let bf3 = build_service_template(&svc, TEST_NAMESPACE, bf3_suffix);
        assert_eq!(bf3.metadata.name.as_deref(), Some(DOCA_HBN_SERVICE_NAME));
        assert_eq!(bf3.spec.deployment_service_name, DOCA_HBN_SERVICE_NAME);

        // GB200 BF3: separate CR name while retaining the same logical service name.
        let bf3_gb200 = build_service_template(&svc, TEST_NAMESPACE, bf3_gb200_suffix);
        assert_eq!(
            bf3_gb200.metadata.name.as_deref(),
            Some("doca-hbn-bf3gb200")
        );
        assert_eq!(
            bf3_gb200.spec.deployment_service_name,
            DOCA_HBN_SERVICE_NAME
        );

        // BF4: CR name suffixed, logical name unchanged.
        let bf4 = build_service_template(&svc, TEST_NAMESPACE, bf4_suffix);
        assert_eq!(
            bf4.metadata.name.as_deref(),
            Some("doca-hbn-bf4generic"),
            "BF4 service CRs must be suffixed to avoid overwriting BF3"
        );
        assert_eq!(bf4.spec.deployment_service_name, DOCA_HBN_SERVICE_NAME);

        // The DPUDeployment map key stays the logical name; the template/config
        // references point at the per-deployment CR name.
        let services = vec![svc];
        let bf4_deployment = build_deployment(
            &services,
            "dep",
            &DpuProvisioningSource::Bfb("bfb".to_string()),
            "flavor",
            TEST_NAMESPACE,
            &[],
            BTreeMap::new(),
            DpuDeploymentType::Bf4Generic,
        );
        let entry = bf4_deployment
            .spec
            .services
            .get(DOCA_HBN_SERVICE_NAME)
            .expect("map key is the logical service name");
        assert_eq!(
            entry.service_template.as_deref(),
            Some("doca-hbn-bf4generic")
        );
        assert_eq!(
            entry.service_configuration.as_deref(),
            Some("doca-hbn-bf4generic")
        );
    }

    #[test]
    fn deployment_type_controls_cr_suffix_and_astra_enablement() {
        value_scenarios!(
            run = |deployment_type| {
                let deployment = build_deployment(
                    &[],
                    "deployment",
                    &DpuProvisioningSource::Bfb("bfb".to_string()),
                    "flavor",
                    TEST_NAMESPACE,
                    &[],
                    BTreeMap::new(),
                    deployment_type,
                );
                (
                    deployment_cr_suffix(deployment_type),
                    deployment.spec.dpus.astra_enabled,
                )
            };
            "BF3 preserves unsuffixed resource names" {
                DpuDeploymentType::Bf3 => ("", None),
            }

            "GB200 BF3 uses its deployment suffix" {
                DpuDeploymentType::Bf3Gb200 => ("bf3gb200", None),
            }

            "generic BF4 uses its deployment suffix" {
                DpuDeploymentType::Bf4Generic => ("bf4generic", None),
            }

            "Astra BF4 uses its deployment suffix and enables Astra" {
                DpuDeploymentType::Bf4Astra => ("bf4astra", Some(true)),
            }
        );
    }

    #[derive(Clone, Default)]
    struct SdkMock {
        devices: Arc<RwLock<BTreeMap<String, DPUDevice>>>,
        nodes: Arc<RwLock<BTreeMap<String, DPUNode>>>,
        dpus: Arc<RwLock<BTreeMap<String, DPU>>>,
        flavors: Arc<RwLock<BTreeMap<String, DPUFlavor>>>,
        flavor_templates: Arc<RwLock<BTreeMap<String, DPUFlavorTemplate>>>,
        services: Arc<RwLock<BTreeMap<String, DPUService>>>,
        service_patch: Arc<RwLock<Option<(String, String, serde_json::Value)>>>,
        node_patches: Arc<RwLock<Vec<serde_json::Value>>>,
        dpu_delete_error: Arc<RwLock<Option<DpfError>>>,
    }

    impl SdkMock {
        fn new() -> Self {
            Self::default()
        }

        fn key<T: Resource>(r: &T) -> String {
            format!(
                "{}/{}",
                r.meta().namespace.as_deref().unwrap_or(""),
                r.meta().name.as_deref().unwrap_or("")
            )
        }

        fn ns_key(ns: &str, name: &str) -> String {
            format!("{}/{}", ns, name)
        }
    }

    #[async_trait]
    impl DpuServiceRepository for SdkMock {
        async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUService>, DpfError> {
            Ok(self
                .services
                .read()
                .unwrap()
                .get(&Self::ns_key(ns, name))
                .cloned())
        }

        async fn list(&self, ns: &str) -> Result<Vec<DPUService>, DpfError> {
            Ok(self
                .services
                .read()
                .unwrap()
                .iter()
                .filter(|(key, _)| key.starts_with(&format!("{ns}/")))
                .map(|(_, service)| service.clone())
                .collect())
        }

        async fn create(&self, service: &DPUService) -> Result<DPUService, DpfError> {
            let key = Self::key(service);
            let mut services = self.services.write().unwrap();
            if services.contains_key(&key) {
                return Err(already_exists_error(
                    service.meta().name.as_deref().unwrap_or(""),
                ));
            }
            services.insert(key, service.clone());
            Ok(service.clone())
        }

        async fn patch(
            &self,
            name: &str,
            ns: &str,
            patch: serde_json::Value,
        ) -> Result<(), DpfError> {
            *self.service_patch.write().unwrap() = Some((name.to_string(), ns.to_string(), patch));
            Ok(())
        }

        async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
            self.services
                .write()
                .unwrap()
                .remove(&Self::ns_key(ns, name));
            Ok(())
        }
    }

    #[async_trait]
    impl crate::repository::DpuDeviceRepository for SdkMock {
        async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUDevice>, DpfError> {
            Ok(self
                .devices
                .read()
                .unwrap()
                .get(&Self::ns_key(ns, name))
                .cloned())
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
            let key = Self::key(d);
            let mut devices = self.devices.write().unwrap();
            if devices.contains_key(&key) {
                return Err(already_exists_error(d.meta().name.as_deref().unwrap_or("")));
            }
            devices.insert(key, d.clone());
            Ok(d.clone())
        }
        async fn patch(
            &self,
            name: &str,
            ns: &str,
            patch: serde_json::Value,
        ) -> Result<(), DpfError> {
            let mut devices = self.devices.write().unwrap();
            let device = devices
                .get_mut(&Self::ns_key(ns, name))
                .ok_or_else(|| DpfError::not_found("DPUDevice", name))?;

            if let Some(values) = patch
                .pointer("/spec/values")
                .and_then(serde_json::Value::as_object)
            {
                device.spec.values = Some(
                    values
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                );
                return Ok(());
            }

            let Some(node_labels) = patch
                .pointer("/spec/cluster/nodeLabels")
                .and_then(serde_json::Value::as_object)
            else {
                return Ok(());
            };

            let cluster = device.spec.cluster.get_or_insert(DpuDeviceCluster {
                node_annotations: None,
                node_labels: None,
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
            self.devices
                .write()
                .unwrap()
                .remove(&Self::ns_key(ns, name));
            Ok(())
        }
    }

    #[async_trait]
    impl crate::repository::DpuNodeRepository for SdkMock {
        async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUNode>, DpfError> {
            Ok(self
                .nodes
                .read()
                .unwrap()
                .get(&Self::ns_key(ns, name))
                .cloned())
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
            let key = Self::key(n);
            let mut nodes = self.nodes.write().unwrap();
            if nodes.contains_key(&key) {
                return Err(already_exists_error(n.meta().name.as_deref().unwrap_or("")));
            }
            nodes.insert(key, n.clone());
            Ok(n.clone())
        }
        async fn patch(
            &self,
            name: &str,
            ns: &str,
            patch: serde_json::Value,
        ) -> Result<(), DpfError> {
            self.node_patches.write().unwrap().push(patch.clone());

            if let Some(node) = self.nodes.write().unwrap().get_mut(&Self::ns_key(ns, name)) {
                if let Some(annos) = patch
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
                if let Some(labels) = patch
                    .pointer("/metadata/labels")
                    .and_then(|v| v.as_object())
                {
                    let node_labels = node.metadata.labels.get_or_insert_with(BTreeMap::new);
                    for (k, v) in labels {
                        if v.is_null() {
                            node_labels.remove(k);
                        } else if let Some(s) = v.as_str() {
                            node_labels.insert(k.clone(), s.to_string());
                        }
                    }
                }
            }
            Ok(())
        }
        async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
            self.nodes.write().unwrap().remove(&Self::ns_key(ns, name));
            Ok(())
        }
    }

    #[async_trait]
    impl crate::repository::DpuRepository for SdkMock {
        async fn get(&self, name: &str, ns: &str) -> Result<Option<DPU>, DpfError> {
            Ok(self
                .dpus
                .read()
                .unwrap()
                .get(&Self::ns_key(ns, name))
                .cloned())
        }
        async fn list(
            &self,
            ns: &str,
            _label_selector: Option<&str>,
        ) -> Result<Vec<DPU>, DpfError> {
            Ok(self
                .dpus
                .read()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with(&format!("{}/", ns)))
                .map(|(_, v)| v.clone())
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
            if let Some(error) = self.dpu_delete_error.write().unwrap().take() {
                return Err(error);
            }
            self.dpus.write().unwrap().remove(&Self::ns_key(ns, name));
            Ok(())
        }
        async fn delete_if_uid(&self, name: &str, ns: &str, uid: &str) -> Result<(), DpfError> {
            let current_uid = self
                .dpus
                .read()
                .unwrap()
                .get(&Self::ns_key(ns, name))
                .map(|dpu| dpu.metadata.uid.clone())
                .ok_or_else(|| not_found_error(name))?;
            if current_uid.as_deref() != Some(uid) {
                return Err(DpfError::InvalidState(format!(
                    "DPU {name} UID changed before deletion"
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
    impl crate::repository::K8sConfigRepository for SdkMock {
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
            _name: &str,
            _ns: &str,
        ) -> Result<Option<BTreeMap<String, String>>, DpfError> {
            Ok(None)
        }
        async fn apply_configmap(
            &self,
            _name: &str,
            _ns: &str,
            _data: BTreeMap<String, String>,
        ) -> Result<(), DpfError> {
            Ok(())
        }
        async fn get_secret(
            &self,
            _name: &str,
            _ns: &str,
        ) -> Result<Option<BTreeMap<String, Vec<u8>>>, DpfError> {
            Ok(None)
        }
        async fn apply_secret(
            &self,
            _name: &str,
            _ns: &str,
            _data: BTreeMap<String, Vec<u8>>,
        ) -> Result<(), DpfError> {
            Ok(())
        }
    }

    #[async_trait]
    impl crate::repository::DpfOperatorConfigRepository for SdkMock {
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

    #[async_trait]
    impl DpuFlavorRepository for SdkMock {
        async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUFlavor>, DpfError> {
            Ok(self
                .flavors
                .read()
                .unwrap()
                .get(&Self::ns_key(ns, name))
                .cloned())
        }
        async fn create(&self, f: &DPUFlavor) -> Result<DPUFlavor, DpfError> {
            let key = Self::key(f);
            let mut flavors = self.flavors.write().unwrap();
            if flavors.contains_key(&key) {
                return Err(already_exists_error(f.meta().name.as_deref().unwrap_or("")));
            }
            flavors.insert(key, f.clone());
            Ok(f.clone())
        }
    }

    #[async_trait]
    impl DpuFlavorTemplateRepository for SdkMock {
        async fn get(&self, name: &str, ns: &str) -> Result<Option<DPUFlavorTemplate>, DpfError> {
            Ok(self
                .flavor_templates
                .read()
                .unwrap()
                .get(&Self::ns_key(ns, name))
                .cloned())
        }
        async fn create(
            &self,
            template: &DPUFlavorTemplate,
        ) -> Result<DPUFlavorTemplate, DpfError> {
            let key = Self::key(template);
            let mut templates = self.flavor_templates.write().unwrap();
            if templates.contains_key(&key) {
                return Err(already_exists_error(
                    template.meta().name.as_deref().unwrap_or(""),
                ));
            }
            templates.insert(key, template.clone());
            Ok(template.clone())
        }
    }

    #[tokio::test]
    async fn test_register_dpu_device() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN123456".to_string(),
            dpu_machine_id: "dpu-bbb".to_string(),
            is_primary: true,
        };

        sdk.register_dpu_device(info, None).await.unwrap();

        let devices = DpuDeviceRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].spec.serial_number, "SN123456");
    }

    #[test]
    fn astra_underlay_values_require_unique_mac_and_ip() {
        let underlay_ip_macs = [
            ("dc:73:fc:21:f8:20", "100.96.0.212"),
            ("dc:73:fc:21:f9:30", "100.97.0.214"),
            ("dc:73:fc:21:f9:20", "100.98.0.216"),
            ("dc:73:fc:21:f8:50", "100.99.0.218"),
            ("dc:73:fc:21:f9:50", "100.104.0.220"),
            ("dc:73:fc:21:f8:40", "100.105.0.222"),
            ("dc:73:fc:21:f9:40", "100.106.0.224"),
            ("dc:73:fc:21:f8:30", "100.107.0.226"),
        ]
        .map(|(mac, ip)| (mac.to_string(), ip.parse::<Ipv4Addr>().unwrap()));

        let values = astra_underlay_values_for_ip_macs(&underlay_ip_macs).unwrap();
        assert_eq!(values.len(), 40);
        assert_eq!(values["mac_0_val"].as_str(), Some("dc:73:fc:21:f8:20"));
        assert_eq!(values["ip_0_val"].as_str(), Some("100.96.0.212/31"));
        assert_eq!(values["gw_0_val"].as_str(), Some("100.96.0.213"));
        assert_eq!(values["route1_0_val"].as_str(), Some("100.96.0.0/16"));
        assert_eq!(values["route2_0_val"].as_str(), Some("100.96.0.0/13"));
        assert_eq!(values["mac_7_val"].as_str(), Some("dc:73:fc:21:f8:30"));
        assert_eq!(values["ip_7_val"].as_str(), Some("100.107.0.226/31"));
        assert_eq!(values["gw_7_val"].as_str(), Some("100.107.0.227"));
        assert_eq!(values["route1_7_val"].as_str(), Some("100.107.0.0/16"));
        assert_eq!(values["route2_7_val"].as_str(), Some("100.104.0.0/13"));
        assert!(astra_underlay_values_for_ip_macs(&underlay_ip_macs[..7]).is_err());

        let mut duplicate_ips = underlay_ip_macs.clone();
        duplicate_ips[7].1 = duplicate_ips[0].1;
        let error = astra_underlay_values_for_ip_macs(&duplicate_ips).unwrap_err();
        assert!(
            matches!(error, DpfError::ConfigError(message) if message == "Astra underlay IPs must be unique")
        );

        let mut duplicate_macs = underlay_ip_macs;
        duplicate_macs[7].0 = duplicate_macs[0].0.clone();
        let error = astra_underlay_values_for_ip_macs(&duplicate_macs).unwrap_err();
        assert!(
            matches!(error, DpfError::ConfigError(message) if message == "Astra underlay MACs must be unique")
        );
    }

    #[test]
    fn astra_dpu_device_values_exactly_match_flavor_template_references() {
        let underlay_ip_macs = [
            ("dc:73:fc:21:f8:20", "100.96.0.212"),
            ("dc:73:fc:21:f9:30", "100.97.0.214"),
            ("dc:73:fc:21:f9:20", "100.98.0.216"),
            ("dc:73:fc:21:f8:50", "100.99.0.218"),
            ("dc:73:fc:21:f9:50", "100.104.0.220"),
            ("dc:73:fc:21:f8:40", "100.105.0.222"),
            ("dc:73:fc:21:f9:40", "100.106.0.224"),
            ("dc:73:fc:21:f8:30", "100.107.0.226"),
        ]
        .map(|(mac, ip)| (mac.to_string(), ip.parse::<Ipv4Addr>().unwrap()));
        let values = astra_underlay_values_for_ip_macs(&underlay_ip_macs).unwrap();
        let value_keys: BTreeSet<_> = values.keys().cloned().collect();

        let template = crate::flavor::flavor_bf4_astra(
            "astra-ns",
            &None,
            calculate_astra_pf_total_sf(build_astra_dpu_interfaces_vec().as_slice()).unwrap(),
            &[],
        )
        .unwrap();
        let reference_keys: BTreeSet<_> = template
            .spec
            .template
            .split("{{ .")
            .skip(1)
            .map(|reference| {
                reference
                    .split_once(" }}")
                    .expect("Astra template reference must be closed")
                    .0
                    .to_owned()
            })
            .collect();

        assert_eq!(reference_keys, value_keys);

        let mut rendered_template = template.spec.template;
        for (key, value) in values {
            rendered_template = rendered_template.replace(
                &format!("{{{{ .{key} }}}}"),
                value.as_str().expect("Astra value must be a string"),
            );
        }
        let rendered: serde_yaml::Value = serde_yaml::from_str(&rendered_template).unwrap();
        let xplane_script = rendered["spec"]["configFiles"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|file| file["path"].as_str() == Some("/etc/mellanox/xplane-bridge.sh"))
            .and_then(|file| file["raw"].as_str())
            .unwrap();

        assert!(xplane_script.contains("bridge_for_mac()"));
        assert!(
            xplane_script
                .contains("/sys/bus/pci/devices/${pci}/net/${iface_val}/smart_nic/pf/config")
        );
        assert!(xplane_script.contains("grep -qiF \"$target_mac\" \"$config_path\""));

        for (mac, ip) in underlay_ip_macs {
            let octets = ip.octets();
            let gateway = Ipv4Addr::from(u32::from(ip) ^ 1);
            assert!(xplane_script.contains(&format!(
                "\"{mac}|{ip}/31|{gateway}|{}.{}.0.0/16|{}.{}.0.0/13\"",
                octets[0],
                octets[1],
                octets[0],
                octets[1] & 0b1111_1000
            )));
        }
    }

    #[tokio::test]
    async fn test_register_dpu_node() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuNodeInfo {
            node_id: "host-001".to_string(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            device_ids: vec!["dpu-001".to_string(), "dpu-002".to_string()],
            deployment_type: DpuDeploymentType::Bf3,
        };

        sdk.register_dpu_node(info).await.unwrap();

        let nodes = DpuNodeRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].metadata.name, Some("node-host-001".to_string()));
        assert_eq!(nodes[0].spec.dpus.as_ref().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_delete_dpu_device() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN123456".to_string(),
            dpu_machine_id: "dpu-bbb".to_string(),
            is_primary: true,
        };

        sdk.register_dpu_device(info, None).await.unwrap();

        let devices = DpuDeviceRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        assert_eq!(devices.len(), 1);

        sdk.delete_dpu_device("dpu-001").await.unwrap();

        let devices = DpuDeviceRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        assert_eq!(devices.len(), 0);
    }

    #[tokio::test]
    async fn test_delete_dpu_node() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuNodeInfo {
            node_id: "host-001".to_string(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            device_ids: vec!["dpu-001".to_string()],
            deployment_type: DpuDeploymentType::Bf3,
        };

        sdk.register_dpu_node(info).await.unwrap();

        let nodes = DpuNodeRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);

        sdk.delete_dpu_node("node-host-001").await.unwrap();

        let nodes = DpuNodeRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 0);
    }

    struct TestLabeler;

    impl ResourceLabeler for TestLabeler {
        fn device_labels(&self, info: &DpuDeviceInfo) -> BTreeMap<String, String> {
            BTreeMap::from([
                ("test/device".to_string(), "true".to_string()),
                ("test/host-bmc-ip".to_string(), info.host_bmc_ip.to_string()),
                (
                    "test/dpu-machine-id".to_string(),
                    info.dpu_machine_id.clone(),
                ),
            ])
        }

        fn node_labels(&self) -> BTreeMap<String, String> {
            BTreeMap::from([("test/node".to_string(), "true".to_string())])
        }

        fn node_labels_for_deployment_type(
            &self,
            _deployment_type: DpuDeploymentType,
        ) -> Result<BTreeMap<String, String>, crate::DpfError> {
            Ok(self.node_labels())
        }

        fn node_context_labels(&self, _info: &DpuNodeInfo) -> BTreeMap<String, String> {
            BTreeMap::new()
        }
    }

    /// Supplies selectors with one shared label and one label unique to each
    /// deployment so transfer tests can distinguish both responsibilities.
    struct DeploymentTransferLabeler;

    impl ResourceLabeler for DeploymentTransferLabeler {
        fn node_labels_for_deployment_type(
            &self,
            deployment_type: DpuDeploymentType,
        ) -> Result<BTreeMap<String, String>, crate::DpfError> {
            let deployment_label = match deployment_type {
                DpuDeploymentType::Bf3 => "test/deployment-bf3",
                DpuDeploymentType::Bf3Gb200 => "test/deployment-bf3gb200",
                DpuDeploymentType::Bf4Generic => "test/deployment-bf4",
                DpuDeploymentType::Bf4Astra => "test/deployment-astra",
            };
            Ok(BTreeMap::from([
                ("test/shared".to_string(), "true".to_string()),
                (deployment_label.to_string(), "true".to_string()),
            ]))
        }
    }

    /// Builds the existing DPUNode used to exercise a deployment label
    /// transfer without involving registration behavior.
    fn dpu_node_for_deployment_transfer() -> DPUNode {
        DPUNode {
            metadata: ObjectMeta {
                name: Some("node-host-001".to_string()),
                namespace: Some(TEST_NAMESPACE.to_string()),
                resource_version: Some("7".to_string()),
                labels: Some(BTreeMap::from([
                    ("test/shared".to_string(), "true".to_string()),
                    ("test/deployment-bf3".to_string(), "true".to_string()),
                    ("test/host".to_string(), "host-001".to_string()),
                    ("external/label".to_string(), "preserved".to_string()),
                ])),
                ..Default::default()
            },
            spec: DpuNodeSpec {
                dpus: Some(vec![]),
                node_dms_address: None,
                node_reboot_method: None,
            },
            status: None,
        }
    }

    /// A deployment transfer removes only the source selector, adds the target
    /// selector, and leaves shared, contextual, and outside labels untouched.
    #[tokio::test]
    async fn deployment_label_transfer_is_idempotent_and_preserves_other_labels() {
        let mock = SdkMock::new();
        let node = dpu_node_for_deployment_transfer();
        mock.nodes
            .write()
            .unwrap()
            .insert(SdkMock::key(&node), node);
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .with_labeler(DeploymentTransferLabeler)
            .build_without_resources()
            .await
            .unwrap();

        sdk.transfer_dpu_node_deployment_labels(
            "node-host-001",
            DpuDeploymentType::Bf3,
            DpuDeploymentType::Bf3Gb200,
        )
        .await
        .unwrap();

        let node = mock
            .nodes
            .read()
            .unwrap()
            .get(&SdkMock::ns_key(TEST_NAMESPACE, "node-host-001"))
            .cloned()
            .unwrap();
        assert_eq!(
            node.metadata.labels,
            Some(BTreeMap::from([
                ("test/shared".to_string(), "true".to_string()),
                ("test/deployment-bf3gb200".to_string(), "true".to_string()),
                ("test/host".to_string(), "host-001".to_string()),
                ("external/label".to_string(), "preserved".to_string()),
            ]))
        );
        assert_eq!(
            mock.node_patches.read().unwrap()[0]
                .pointer("/metadata/resourceVersion")
                .and_then(serde_json::Value::as_str),
            Some("7")
        );

        sdk.transfer_dpu_node_deployment_labels(
            "node-host-001",
            DpuDeploymentType::Bf3,
            DpuDeploymentType::Bf3Gb200,
        )
        .await
        .unwrap();
        assert_eq!(mock.node_patches.read().unwrap().len(), 1);

        // Repair a node that was left matching both deployments by removing
        // the source-only selector even though the target already matches.
        mock.nodes
            .write()
            .unwrap()
            .get_mut(&SdkMock::ns_key(TEST_NAMESPACE, "node-host-001"))
            .unwrap()
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert("test/deployment-bf3".to_string(), "true".to_string());
        sdk.transfer_dpu_node_deployment_labels(
            "node-host-001",
            DpuDeploymentType::Bf3,
            DpuDeploymentType::Bf3Gb200,
        )
        .await
        .unwrap();

        let node = mock
            .nodes
            .read()
            .unwrap()
            .get(&SdkMock::ns_key(TEST_NAMESPACE, "node-host-001"))
            .cloned()
            .unwrap();
        assert_eq!(
            node.metadata.labels,
            Some(BTreeMap::from([
                ("test/shared".to_string(), "true".to_string()),
                ("test/deployment-bf3gb200".to_string(), "true".to_string()),
                ("test/host".to_string(), "host-001".to_string()),
                ("external/label".to_string(), "preserved".to_string()),
            ]))
        );
        assert_eq!(mock.node_patches.read().unwrap().len(), 2);
    }

    /// A DPUNode outside both selectors cannot be claimed by the target
    /// deployment through the transfer operation.
    #[tokio::test]
    async fn deployment_label_transfer_rejects_unrelated_node() {
        let mock = SdkMock::new();
        let mut node = dpu_node_for_deployment_transfer();
        node.metadata.labels = Some(BTreeMap::from([
            ("test/shared".to_string(), "true".to_string()),
            ("external/label".to_string(), "preserved".to_string()),
        ]));
        mock.nodes
            .write()
            .unwrap()
            .insert(SdkMock::key(&node), node);
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .with_labeler(DeploymentTransferLabeler)
            .build_without_resources()
            .await
            .unwrap();

        let error = sdk
            .transfer_dpu_node_deployment_labels(
                "node-host-001",
                DpuDeploymentType::Bf3,
                DpuDeploymentType::Bf3Gb200,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, DpfError::InvalidState(_)));
        assert!(mock.node_patches.read().unwrap().is_empty());
    }

    /// Empty selectors cannot authorize a deployment transfer, even though an
    /// empty map would otherwise match every DPUNode.
    #[tokio::test]
    async fn deployment_label_transfer_rejects_empty_selectors() {
        let mock = SdkMock::new();
        let node = dpu_node_for_deployment_transfer();
        mock.nodes
            .write()
            .unwrap()
            .insert(SdkMock::key(&node), node);
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let error = sdk
            .transfer_dpu_node_deployment_labels(
                "node-host-001",
                DpuDeploymentType::Bf3,
                DpuDeploymentType::Bf3Gb200,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, DpfError::ConfigError(_)));
        assert!(mock.node_patches.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_dpu_device_info_labels() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .with_labeler(TestLabeler)
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN123456".to_string(),
            dpu_machine_id: "dpu-bbb".to_string(),
            is_primary: true,
        };

        sdk.register_dpu_device(info, None).await.unwrap();

        let devices = DpuDeviceRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        let device = &devices[0];
        let labels = device.metadata.labels.as_ref().unwrap();

        assert_eq!(labels.get("test/device"), Some(&"true".to_string()));
        assert_eq!(
            labels.get("test/host-bmc-ip"),
            Some(&"10.0.0.1".to_string())
        );
        assert_eq!(
            labels.get("test/dpu-machine-id"),
            Some(&"dpu-bbb".to_string())
        );
    }

    #[tokio::test]
    async fn test_dpu_device_no_labels_without_labeler() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN123456".to_string(),
            dpu_machine_id: "dpu-bbb".to_string(),
            is_primary: true,
        };

        sdk.register_dpu_device(info, None).await.unwrap();

        let devices = DpuDeviceRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        let device = &devices[0];
        assert!(device.metadata.labels.is_none());
    }

    #[tokio::test]
    async fn test_dpu_node_labels() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .with_labeler(TestLabeler)
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuNodeInfo {
            node_id: "host-001".to_string(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            device_ids: vec!["dpu-001".to_string()],
            deployment_type: DpuDeploymentType::Bf3,
        };

        sdk.register_dpu_node(info).await.unwrap();

        let nodes = DpuNodeRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        let node = &nodes[0];
        let labels = node.metadata.labels.as_ref().unwrap();

        assert_eq!(labels.get("test/node"), Some(&"true".to_string()));
    }

    #[tokio::test]
    async fn test_dpu_node_no_labels_without_labeler() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let info = DpuNodeInfo {
            node_id: "host-001".to_string(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            device_ids: vec!["dpu-001".to_string()],
            deployment_type: DpuDeploymentType::Bf3,
        };

        sdk.register_dpu_node(info).await.unwrap();

        let nodes = DpuNodeRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        let node = &nodes[0];
        assert!(node.metadata.labels.is_none());
    }

    #[tokio::test]
    async fn test_node_label_removal_patch_contains_labeler_keys() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock, TEST_NAMESPACE, String::new())
            .with_labeler(TestLabeler)
            .build_without_resources()
            .await
            .unwrap();

        let patch = sdk.node_label_removal_patch();
        let labels = patch
            .pointer("/metadata/labels")
            .unwrap()
            .as_object()
            .unwrap();

        assert!(labels.contains_key("test/node"));
        assert!(labels["test/node"].is_null());
    }

    #[tokio::test]
    async fn test_node_label_removal_patch_empty_without_labeler() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock, TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let patch = sdk.node_label_removal_patch();
        let labels = patch
            .pointer("/metadata/labels")
            .unwrap()
            .as_object()
            .unwrap();

        assert!(labels.is_empty());
    }

    #[tokio::test]
    async fn test_reprovision_dpu_deletes_dpu_not_device() {
        use kube::core::ObjectMeta;

        use crate::crds::dpus_generated::{DpuSpec, DpuStatus, DpuStatusPhase};

        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let device_info = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN123".to_string(),
            dpu_machine_id: "dpu-bbb".to_string(),
            is_primary: true,
        };
        sdk.register_dpu_device(device_info, None).await.unwrap();

        let dpu_name = "node-dpu-001-device-dpu-001";
        let dpu = DPU {
            metadata: ObjectMeta {
                name: Some(dpu_name.to_string()),
                namespace: Some(TEST_NAMESPACE.to_string()),
                ..Default::default()
            },
            spec: DpuSpec {
                bfb: Some("bf-bundle".to_string()),
                bmc_ip: None,
                cluster: None,
                dpu_device_name: "dpu-001".to_string(),
                dpu_flavor: crate::flavor::DEFAULT_FLAVOR_NAME.to_string(),
                dpu_node_name: "node-dpu-001".to_string(),
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
                serial_number: "SN123".to_string(),
                blue_field_software: None,
                secure_boot: None,
                astra_enabled: None,
            },
            status: Some(DpuStatus {
                phase: DpuStatusPhase::Ready,
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
            }),
        };
        mock.dpus
            .write()
            .unwrap()
            .insert(format!("{}/{}", TEST_NAMESPACE, dpu_name), dpu);

        sdk.reprovision_dpu("dpu-001", "node-dpu-001")
            .await
            .unwrap();

        let dpus = DpuRepository::list(&mock, TEST_NAMESPACE, None)
            .await
            .unwrap();
        assert_eq!(dpus.len(), 0, "DPU CR should be deleted");

        let devices = DpuDeviceRepository::list(&mock, TEST_NAMESPACE)
            .await
            .unwrap();
        assert_eq!(devices.len(), 1, "DPUDevice should remain");
    }

    /// Reprovision suppresses only the error for a missing DPU so retries are
    /// safe after the deterministic DPU CR has already been deleted.
    #[tokio::test]
    async fn reprovision_dpu_suppresses_only_not_found() {
        use carbide_test_support::Outcome::{Fails, Yields};
        use carbide_test_support::{Case, check_cases_async};

        /// Input for checking which DPU deletion errors reprovision suppresses.
        struct ReprovisionDpuInput {
            /// Error returned by the mock DPU repository.
            delete_error: DpfError,
        }

        let run = |input: ReprovisionDpuInput| async move {
            let mock = SdkMock::new();
            *mock.dpu_delete_error.write().unwrap() = Some(input.delete_error);
            let sdk = DpfSdkBuilder::new(mock, TEST_NAMESPACE, String::new())
                .build_without_resources()
                .await
                .unwrap();

            sdk.reprovision_dpu("dpu-001", "node-host-001")
                .await
                .map_err(|error| error.to_string())
        };

        check_cases_async(
            [
                Case {
                    scenario: "DPU was already deleted",
                    input: ReprovisionDpuInput {
                        delete_error: not_found_error("node-host-001-device-dpu-001"),
                    },
                    expect: Yields(()),
                },
                Case {
                    scenario: "DPU deletion failed",
                    input: ReprovisionDpuInput {
                        delete_error: DpfError::InvalidState("delete failed".to_string()),
                    },
                    expect: Fails,
                },
            ],
            run,
        )
        .await;
    }

    #[tokio::test]
    async fn test_get_dpu_phase_reports_deleting_when_terminating() {
        use kube::core::ObjectMeta;

        use crate::crds::dpus_generated::{DpuSpec, DpuStatus, DpuStatusPhase};

        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        // A DPU that has been deleted (reprovision) but whose finalizer has not yet
        // run: it carries a deletionTimestamp while its status.phase is still Ready.
        let dpu_name = "node-dpu-001-device-dpu-001";
        let dpu = DPU {
            metadata: ObjectMeta {
                name: Some(dpu_name.to_string()),
                namespace: Some(TEST_NAMESPACE.to_string()),
                deletion_timestamp: Some(terminating_timestamp()),
                ..Default::default()
            },
            spec: DpuSpec {
                bfb: Some("bf-bundle".to_string()),
                bmc_ip: None,
                cluster: None,
                dpu_device_name: "dpu-001".to_string(),
                dpu_flavor: crate::flavor::DEFAULT_FLAVOR_NAME.to_string(),
                dpu_node_name: "node-dpu-001".to_string(),
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
                serial_number: "SN123".to_string(),
                blue_field_software: None,
                secure_boot: None,
                astra_enabled: None,
            },
            status: Some(DpuStatus {
                phase: DpuStatusPhase::Ready,
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
            }),
        };
        mock.dpus
            .write()
            .unwrap()
            .insert(format!("{}/{}", TEST_NAMESPACE, dpu_name), dpu);

        let phase = sdk.get_dpu_phase("dpu-001", "node-dpu-001").await.unwrap();
        assert_eq!(
            phase,
            DpuPhase::Deleting,
            "a DPU with a deletionTimestamp must report Deleting even though its stale status.phase is Ready"
        );
    }

    #[tokio::test]
    async fn test_namespace_isolation() {
        let mock = SdkMock::new();

        let sdk1 = DpfSdkBuilder::new(mock.clone(), "namespace-1", String::new())
            .build_without_resources()
            .await
            .unwrap();
        let sdk2 = DpfSdkBuilder::new(mock.clone(), "namespace-2", String::new())
            .build_without_resources()
            .await
            .unwrap();

        let info1 = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN111".to_string(),
            dpu_machine_id: "dpu-111".to_string(),
            is_primary: true,
        };

        let info2 = DpuDeviceInfo {
            device_id: "dpu-002".to_string(),
            dpu_bmc_ip: "10.0.0.20".parse().unwrap(),
            host_bmc_ip: "10.0.0.2".parse().unwrap(),
            serial_number: "SN222".to_string(),
            dpu_machine_id: "dpu-222".to_string(),
            is_primary: false,
        };

        sdk1.register_dpu_device(info1, None).await.unwrap();
        sdk2.register_dpu_device(info2, None).await.unwrap();

        let devices1 = DpuDeviceRepository::list(&mock, "namespace-1")
            .await
            .unwrap();
        let devices2 = DpuDeviceRepository::list(&mock, "namespace-2")
            .await
            .unwrap();

        assert_eq!(devices1.len(), 1);
        assert_eq!(devices2.len(), 1);
        assert_eq!(devices1[0].spec.serial_number, "SN111");
        assert_eq!(devices2[0].spec.serial_number, "SN222");
    }

    #[tokio::test]
    async fn merge_dpu_device_node_labels_preserves_unrelated_labels() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let device_name = dpu_device_cr_name("dpu-001");
        let device = DPUDevice {
            metadata: ObjectMeta {
                name: Some(device_name.clone()),
                namespace: Some(TEST_NAMESPACE.to_string()),
                ..Default::default()
            },
            spec: DpuDeviceSpec {
                bmc_ip: None,
                bmc_port: None,
                number_of_p_fs: None,
                opn: None,
                pf0_name: None,
                psid: None,
                serial_number: "SN123456".to_string(),
                bmc_credential_secret_name: None,
                cluster: Some(DpuDeviceCluster {
                    node_annotations: Some(BTreeMap::from([(
                        "other-annotation".to_string(),
                        "other-value".to_string(),
                    )])),
                    node_labels: Some(BTreeMap::from([
                        ("other-controller".to_string(), "preserve".to_string()),
                        ("nico/extsvc-remove".to_string(), "enabled".to_string()),
                    ])),
                }),
                nic_device_count: None,
                values: None,
            },
            status: None,
        };
        DpuDeviceRepository::create(&mock, &device).await.unwrap();

        sdk.merge_dpu_device_node_labels(
            "dpu-001",
            BTreeMap::from([
                ("nico/extsvc-add".to_string(), Some("enabled".to_string())),
                ("nico/extsvc-remove".to_string(), None),
            ]),
        )
        .await
        .unwrap();
        // Reapplying the same patch is the normal Ready-loop retry path and
        // must leave the resource in the same converged state.
        sdk.merge_dpu_device_node_labels(
            "dpu-001",
            BTreeMap::from([
                ("nico/extsvc-add".to_string(), Some("enabled".to_string())),
                ("nico/extsvc-remove".to_string(), None),
            ]),
        )
        .await
        .unwrap();

        assert_eq!(
            sdk.get_dpu_device_node_labels("dpu-001").await.unwrap(),
            BTreeMap::from([
                ("nico/extsvc-add".to_string(), "enabled".to_string()),
                ("other-controller".to_string(), "preserve".to_string()),
            ])
        );

        let updated = DpuDeviceRepository::get(&mock, &device_name, TEST_NAMESPACE)
            .await
            .unwrap()
            .unwrap();
        let cluster = updated.spec.cluster.unwrap();
        assert_eq!(
            cluster.node_annotations,
            Some(BTreeMap::from([(
                "other-annotation".to_string(),
                "other-value".to_string(),
            )]))
        );
        assert_eq!(
            cluster.node_labels,
            Some(BTreeMap::from([
                ("nico/extsvc-add".to_string(), "enabled".to_string()),
                ("other-controller".to_string(), "preserve".to_string()),
            ]))
        );
    }

    #[derive(Clone, Default)]
    struct SecretTrackingMock {
        secrets_written: Arc<std::sync::Mutex<Vec<String>>>,
        fail_writes: bool,
    }

    #[async_trait]
    impl crate::repository::K8sConfigRepository for SecretTrackingMock {
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
            _name: &str,
            _ns: &str,
            data: BTreeMap<String, Vec<u8>>,
        ) -> Result<(), DpfError> {
            if self.fail_writes {
                return Err(DpfError::ConfigError("simulated write failure".into()));
            }
            if let Some(pw_bytes) = data.get("password") {
                let pw = String::from_utf8(pw_bytes.clone()).unwrap();
                self.secrets_written.lock().unwrap().push(pw);
            }
            Ok(())
        }
    }

    #[async_trait]
    impl crate::repository::DpfOperatorConfigRepository for SecretTrackingMock {
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

    /// Provider that always fails, standing in for a site where the site-wide
    /// BMC root credential has not been set yet.
    struct UnsetBmcPasswordProvider;

    #[async_trait]
    impl BmcPasswordProvider for UnsetBmcPasswordProvider {
        async fn get_bmc_password(&self) -> Result<String, DpfError> {
            Err(DpfError::InvalidState(
                "Site wide BMC root credentials not set".into(),
            ))
        }
    }

    #[tokio::test]
    async fn test_refresh_writes_secret_when_password_changes() {
        let mock = SecretTrackingMock::default();
        let provider = "new-password".to_string();

        let result = refresh_bmc_secret_if_changed(
            &mock,
            TEST_NAMESPACE,
            &provider,
            Some("old-password".into()),
        )
        .await;

        assert_eq!(result.as_deref(), Some("new-password"));
        assert_eq!(
            mock.secrets_written.lock().unwrap().as_slice(),
            &["new-password"]
        );
    }

    #[tokio::test]
    async fn test_refresh_skips_write_when_password_unchanged() {
        let mock = SecretTrackingMock::default();
        let provider = "same".to_string();

        let result =
            refresh_bmc_secret_if_changed(&mock, TEST_NAMESPACE, &provider, Some("same".into()))
                .await;

        assert_eq!(result.as_deref(), Some("same"));
        assert!(mock.secrets_written.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_refresh_retains_last_password_on_write_failure() {
        let mock = SecretTrackingMock {
            fail_writes: true,
            ..Default::default()
        };
        let provider = "new-password".to_string();

        let result = refresh_bmc_secret_if_changed(
            &mock,
            TEST_NAMESPACE,
            &provider,
            Some("old-password".into()),
        )
        .await;

        assert_eq!(result.as_deref(), Some("old-password"));
    }

    /// The recovery path for a site that booted before the site-wide BMC root
    /// credential was set: nothing has been written yet, so the first
    /// successful read must write the Secret.
    #[tokio::test]
    async fn test_refresh_writes_secret_when_no_password_written_yet() {
        let mock = SecretTrackingMock::default();
        let provider = "first-password".to_string();

        let result = refresh_bmc_secret_if_changed(&mock, TEST_NAMESPACE, &provider, None).await;

        assert_eq!(result.as_deref(), Some("first-password"));
        assert_eq!(
            mock.secrets_written.lock().unwrap().as_slice(),
            &["first-password"]
        );
    }

    #[tokio::test]
    async fn test_refresh_stays_unwritten_while_password_unavailable() {
        let mock = SecretTrackingMock::default();

        let result =
            refresh_bmc_secret_if_changed(&mock, TEST_NAMESPACE, &UnsetBmcPasswordProvider, None)
                .await;

        assert_eq!(result, None);
        assert!(mock.secrets_written.lock().unwrap().is_empty());
    }

    /// A fresh site sets the site-wide BMC root credential *through* the API,
    /// so initialization must survive the credential being unset, leaving the
    /// Secret to the refresh task.
    #[tokio::test]
    async fn test_build_succeeds_when_bmc_password_unset_and_refresh_configured() {
        let mock = SecretTrackingMock::default();

        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, UnsetBmcPasswordProvider)
            .with_bmc_password_refresh_interval(Duration::from_secs(3600))
            .build_without_resources()
            .await
            .expect("initialization tolerates an unset BMC password");

        assert_eq!(sdk.namespace(), TEST_NAMESPACE);
        assert!(mock.secrets_written.lock().unwrap().is_empty());
    }

    /// Without a refresh task nothing would ever retry the read, so an unset
    /// credential stays fatal rather than leaving the Secret permanently absent.
    #[tokio::test]
    async fn test_build_fails_when_bmc_password_unset_and_no_refresh_configured() {
        let mock = SecretTrackingMock::default();

        let Err(error) = DpfSdkBuilder::new(mock, TEST_NAMESPACE, UnsetBmcPasswordProvider)
            .build_without_resources()
            .await
        else {
            panic!("an unset BMC password with no refresh task is fatal");
        };

        assert!(
            matches!(error, DpfError::InvalidState(msg) if msg.contains("BMC root credentials")),
            "unexpected error"
        );
    }

    fn test_dpu_service(name: &str) -> DetachedDpuServiceDefinition {
        DetachedDpuServiceDefinition {
            name: name.to_owned(),
            namespace: "another-namespace".to_owned(),
            labels: BTreeMap::from([("nico/extension-service-id".to_owned(), "id".to_owned())]),
            helm_chart: DetachedHelmChart {
                repo_url: "oci://registry.example.com/extensions".to_owned(),
                chart: "extension".to_owned(),
                version: "1.0.0".to_owned(),
                release_name: "extension-release".to_owned(),
                values: Some(BTreeMap::from([("replicas".to_owned(), json!(1))])),
            },
            deploy_in_cluster: false,
            security_privileged: false,
            node_selector_labels: BTreeMap::from([(
                "nico/extension-service".to_owned(),
                "enabled".to_owned(),
            )]),
        }
    }

    #[tokio::test]
    async fn dpu_service_lifecycle_delegates_to_repository() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();
        let service = test_dpu_service("extension-service");

        let created = sdk.create_dpu_service(&service).await.unwrap();
        assert_eq!(created.name.as_deref(), Some("extension-service"));
        assert_eq!(created.namespace.as_deref(), Some(TEST_NAMESPACE));
        let resource = mock
            .services
            .read()
            .unwrap()
            .get(&SdkMock::ns_key(TEST_NAMESPACE, "extension-service"))
            .cloned()
            .unwrap();
        assert_eq!(
            serde_json::to_value(resource).unwrap(),
            json!({
                "apiVersion": "svc.dpu.nvidia.com/v1alpha1",
                "kind": "DPUService",
                "metadata": {
                    "labels": {"nico/extension-service-id": "id"},
                    "name": "extension-service",
                    "namespace": TEST_NAMESPACE,
                },
                "spec": {
                    "deployInCluster": false,
                    "helmChart": {
                        "source": {
                            "chart": "extension",
                            "releaseName": "extension-release",
                            "repoURL": "oci://registry.example.com/extensions",
                            "version": "1.0.0",
                        },
                        "values": {"replicas": 1},
                    },
                    "security": {"privileged": false},
                    "serviceDaemonSet": {
                        "nodeSelector": {
                            "nodeSelectorTerms": [{
                                "matchExpressions": [{
                                    "key": "nico/extension-service",
                                    "operator": "In",
                                    "values": ["enabled"],
                                }],
                            }],
                        },
                    },
                },
            })
        );
        assert!(
            !sdk.get_dpu_service("extension-service")
                .await
                .unwrap()
                .expect("created service can be observed")
                .is_deleting
        );
        mock.services
            .write()
            .unwrap()
            .get_mut(&SdkMock::ns_key(TEST_NAMESPACE, "extension-service"))
            .expect("created service is retained by the mock")
            .metadata
            .deletion_timestamp = Some(terminating_timestamp());
        assert!(
            sdk.get_dpu_service("extension-service")
                .await
                .unwrap()
                .expect("finalizer-held service can be observed")
                .is_deleting
        );

        let patch = serde_json::json!({"spec": {"paused": true}});
        sdk.patch_dpu_service("extension-service", patch.clone())
            .await
            .unwrap();
        assert_eq!(
            *mock.service_patch.read().unwrap(),
            Some((
                "extension-service".to_string(),
                TEST_NAMESPACE.to_string(),
                patch,
            ))
        );

        sdk.delete_dpu_service("extension-service").await.unwrap();
        assert!(
            sdk.get_dpu_service("extension-service")
                .await
                .unwrap()
                .is_none()
        );
    }

    fn terminating_timestamp() -> k8s_openapi::apimachinery::pkg::apis::meta::v1::Time {
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            k8s_openapi::jiff::Timestamp::UNIX_EPOCH,
        )
    }

    #[tokio::test]
    async fn test_register_dpu_device_fails_when_terminating() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let terminating_device = DPUDevice {
            metadata: ObjectMeta {
                name: Some(dpu_device_cr_name("dpu-001")),
                namespace: Some(TEST_NAMESPACE.to_string()),
                deletion_timestamp: Some(terminating_timestamp()),
                ..Default::default()
            },
            spec: DpuDeviceSpec {
                bmc_ip: Some("10.0.0.10".to_string()),
                bmc_port: Some(443),
                number_of_p_fs: Some(1),
                opn: None,
                pf0_name: None,
                psid: None,
                serial_number: "SN123456".to_string(),
                bmc_credential_secret_name: None,
                cluster: None,
                nic_device_count: None,
                values: None,
            },
            status: None,
        };
        mock.devices
            .write()
            .unwrap()
            .insert(SdkMock::key(&terminating_device), terminating_device);

        let info = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN123456".to_string(),
            dpu_machine_id: "dpu-bbb".to_string(),
            is_primary: true,
        };
        let err = sdk.register_dpu_device(info, None).await.unwrap_err();
        assert!(
            matches!(err, DpfError::InvalidState(_)),
            "expected InvalidState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_register_dpu_device_ok_when_existing_not_terminating() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let existing_device = DPUDevice {
            metadata: ObjectMeta {
                name: Some(dpu_device_cr_name("dpu-001")),
                namespace: Some(TEST_NAMESPACE.to_string()),
                ..Default::default()
            },
            spec: DpuDeviceSpec {
                bmc_ip: Some("10.0.0.10".to_string()),
                bmc_port: Some(443),
                number_of_p_fs: Some(1),
                opn: None,
                pf0_name: None,
                psid: None,
                serial_number: "SN123456".to_string(),
                bmc_credential_secret_name: None,
                cluster: None,
                nic_device_count: None,
                values: Some(BTreeMap::new()),
            },
            status: None,
        };
        mock.devices
            .write()
            .unwrap()
            .insert(SdkMock::key(&existing_device), existing_device);

        let info = DpuDeviceInfo {
            device_id: "dpu-001".to_string(),
            dpu_bmc_ip: "10.0.0.10".parse().unwrap(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            serial_number: "SN123456".to_string(),
            dpu_machine_id: "dpu-bbb".to_string(),
            is_primary: true,
        };
        // An existing DPUDevice is left untouched, so a retry does not need a
        // complete Astra NIC snapshot just to re-validate creation-only values.
        sdk.register_dpu_device(info, Some(vec![])).await.unwrap();

        // This branch is a deliberate no-op: an existing, non-terminating device is left
        // alone. `.unwrap()` only said no error came back -- assert no second device was
        // created alongside it, which is the whole of what "left alone" means here.
        assert_eq!(mock.devices.read().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_register_dpu_node_fails_when_terminating() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let node_name = dpu_node_cr_name("host-001");
        let terminating_node = DPUNode {
            metadata: ObjectMeta {
                name: Some(node_name.clone()),
                namespace: Some(TEST_NAMESPACE.to_string()),
                deletion_timestamp: Some(terminating_timestamp()),
                ..Default::default()
            },
            spec: DpuNodeSpec {
                dpus: Some(vec![]),
                node_dms_address: None,
                node_reboot_method: None,
            },
            status: None,
        };
        mock.nodes
            .write()
            .unwrap()
            .insert(SdkMock::key(&terminating_node), terminating_node);

        let info = DpuNodeInfo {
            node_id: "host-001".to_string(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            device_ids: vec!["dpu-001".to_string()],
            deployment_type: DpuDeploymentType::Bf3,
        };
        let err = sdk.register_dpu_node(info).await.unwrap_err();
        assert!(
            matches!(err, DpfError::InvalidState(_)),
            "expected InvalidState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_register_dpu_node_ok_when_existing_not_terminating() {
        let mock = SdkMock::new();
        let sdk = DpfSdkBuilder::new(mock.clone(), TEST_NAMESPACE, String::new())
            .build_without_resources()
            .await
            .unwrap();

        let node_name = dpu_node_cr_name("host-001");
        let existing_node = DPUNode {
            metadata: ObjectMeta {
                name: Some(node_name.clone()),
                namespace: Some(TEST_NAMESPACE.to_string()),
                ..Default::default()
            },
            spec: DpuNodeSpec {
                dpus: Some(vec![]),
                node_dms_address: None,
                node_reboot_method: None,
            },
            status: None,
        };
        mock.nodes
            .write()
            .unwrap()
            .insert(SdkMock::key(&existing_node), existing_node);

        let info = DpuNodeInfo {
            node_id: "host-001".to_string(),
            host_bmc_ip: "10.0.0.1".parse().unwrap(),
            device_ids: vec!["dpu-001".to_string()],
            deployment_type: DpuDeploymentType::Bf3,
        };
        sdk.register_dpu_node(info).await.unwrap();

        // Same no-op branch for nodes.
        assert_eq!(mock.nodes.read().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_create_dpu_flavor_fresh() {
        let mock = SdkMock::new();
        let config = flavor_test_config(None);
        let resolved = resolve_initialization_inventory(&config).unwrap();
        let name = create_dpu_flavor(&mock, TEST_NAMESPACE, &config, &resolved)
            .await
            .unwrap();

        // Returned name should have the expected "<prefix>-<hex>" shape.
        assert!(
            name.starts_with(crate::flavor::DEFAULT_FLAVOR_NAME),
            "flavor name should start with the default prefix; got {name}"
        );
        assert_ne!(
            name,
            crate::flavor::DEFAULT_FLAVOR_NAME,
            "name must include hash suffix"
        );

        // The flavor must actually be stored in the mock.
        let stored = DpuFlavorRepository::get(&mock, &name, TEST_NAMESPACE)
            .await
            .unwrap();
        assert!(stored.is_some(), "created flavor should be retrievable");
    }

    #[tokio::test]
    async fn test_create_dpu_flavor_fresh_with_proxy() {
        let mock = SdkMock::new();
        let proxy = Some(DpfProxyDetails {
            https_proxy: "http://proxy.corp:3128".to_string(),
            no_proxy: vec!["10.0.0.0/8".to_string()],
        });
        let config = flavor_test_config(proxy);
        let resolved = resolve_initialization_inventory(&config).unwrap();
        let name_with_proxy = create_dpu_flavor(&mock, TEST_NAMESPACE, &config, &resolved)
            .await
            .unwrap();

        // Proxy flavor must get a different hash than the no-proxy flavor.
        let name_no_proxy = {
            let f = crate::flavor::default_flavor(TEST_NAMESPACE, &None).unwrap();
            f.unique_name(crate::flavor::DEFAULT_FLAVOR_NAME).unwrap()
        };
        assert_ne!(
            name_with_proxy, name_no_proxy,
            "proxy and no-proxy flavors must produce distinct names"
        );
    }

    #[tokio::test]
    async fn test_create_dpu_flavor_disappeared_after_conflict() {
        // Simulate a race: create() returns AlreadyExists but the flavor is gone by the time we
        // call get() — the function should return InvalidState rather than panic.

        // We need a mock whose create() always returns AlreadyExists but get() returns None.
        // Achieve this by pre-inserting then removing the flavor so the key is gone, and
        // instead rely on the fact that SdkMock::create returns AlreadyExists only when the
        // key is present — so we simply insert a *different* key so create conflicts but the
        // flavor we look up is absent.
        //
        // Easier: insert the flavor under a wrong key so `create` finds a key collision on the
        // real key (it won't), or just verify the existing None branch indirectly.
        //
        // The cleanest approach: build a custom mock that always errors on create.
        #[derive(Clone, Default)]
        struct AlwaysConflictsMock {
            // empty — get() will always return None
        }

        #[async_trait::async_trait]
        impl DpuFlavorRepository for AlwaysConflictsMock {
            async fn get(&self, _name: &str, _ns: &str) -> Result<Option<DPUFlavor>, DpfError> {
                Ok(None)
            }
            async fn create(&self, f: &DPUFlavor) -> Result<DPUFlavor, DpfError> {
                Err(already_exists_error(f.meta().name.as_deref().unwrap_or("")))
            }
        }

        let mock = AlwaysConflictsMock::default();
        let config = flavor_test_config(None);
        let resolved = resolve_initialization_inventory(&config).unwrap();
        let err = create_dpu_flavor(&mock, TEST_NAMESPACE, &config, &resolved)
            .await
            .unwrap_err();

        assert!(
            matches!(err, DpfError::InvalidState(_)),
            "expected InvalidState when flavor disappears after conflict; got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_dpu_flavor_fails_when_terminating() {
        let mock = SdkMock::new();
        let mut flavor = crate::flavor::default_flavor(TEST_NAMESPACE, &None).unwrap();
        // Use the hash-derived name so the mock key matches what create_dpu_flavor will use.
        let hash_name = flavor
            .unique_name(crate::flavor::DEFAULT_FLAVOR_NAME)
            .unwrap();
        flavor.metadata.name = Some(hash_name);
        let mut terminating_flavor = flavor.clone();
        terminating_flavor.metadata.deletion_timestamp = Some(terminating_timestamp());
        mock.flavors
            .write()
            .unwrap()
            .insert(SdkMock::key(&terminating_flavor), terminating_flavor);

        let config = flavor_test_config(None);
        let resolved = resolve_initialization_inventory(&config).unwrap();
        let err = create_dpu_flavor(&mock, TEST_NAMESPACE, &config, &resolved)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DpfError::InvalidState(_)),
            "expected InvalidState, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_create_dpu_flavor_ok_when_existing_not_terminating() {
        let mock = SdkMock::new();
        let mut flavor = crate::flavor::default_flavor(TEST_NAMESPACE, &None).unwrap();
        // Use the hash-derived name so the mock key matches what create_dpu_flavor will use.
        flavor.metadata.name = Some(
            flavor
                .unique_name(crate::flavor::DEFAULT_FLAVOR_NAME)
                .unwrap(),
        );
        let flavor_name = flavor.metadata.name.clone().unwrap();
        mock.flavors
            .write()
            .unwrap()
            .insert(SdkMock::key(&flavor), flavor);

        let expected_name = flavor_name.clone();
        let config = flavor_test_config(None);
        let resolved = resolve_initialization_inventory(&config).unwrap();
        let returned = create_dpu_flavor(&mock, TEST_NAMESPACE, &config, &resolved)
            .await
            .unwrap();

        // The whole contract of this branch is "reuse what's already there". `.unwrap()`
        // only proved it didn't error -- so check it hands back the existing flavor's name
        // and, more to the point, that it didn't quietly create a second one alongside it.
        assert_eq!(returned, expected_name);
        assert_eq!(mock.flavors.read().unwrap().len(), 1);
    }

    #[derive(Clone, Default)]
    struct BfbMock {
        bfbs: Arc<RwLock<BTreeMap<String, BFB>>>,
    }

    #[async_trait]
    impl crate::repository::BfbRepository for BfbMock {
        async fn get(&self, name: &str, ns: &str) -> Result<Option<BFB>, DpfError> {
            Ok(self
                .bfbs
                .read()
                .unwrap()
                .get(&format!("{ns}/{name}"))
                .cloned())
        }
        async fn list(&self, ns: &str) -> Result<Vec<BFB>, DpfError> {
            let prefix = format!("{ns}/");
            Ok(self
                .bfbs
                .read()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(_, v)| v.clone())
                .collect())
        }
        async fn create(&self, bfb: &BFB) -> Result<BFB, DpfError> {
            let key = format!(
                "{}/{}",
                bfb.meta().namespace.as_deref().unwrap_or(""),
                bfb.meta().name.as_deref().unwrap_or("")
            );
            let mut store = self.bfbs.write().unwrap();
            if store.contains_key(&key) {
                return Err(already_exists_error(
                    bfb.meta().name.as_deref().unwrap_or(""),
                ));
            }
            store.insert(key, bfb.clone());
            Ok(bfb.clone())
        }
        async fn delete(&self, name: &str, ns: &str) -> Result<(), DpfError> {
            self.bfbs.write().unwrap().remove(&format!("{ns}/{name}"));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_create_bfb_deterministic_name() {
        let url = "http://example.com/some.bfb";
        let name1 = create_bfb(&BfbMock::default(), TEST_NAMESPACE, url)
            .await
            .unwrap();
        let name2 = create_bfb(&BfbMock::default(), TEST_NAMESPACE, url)
            .await
            .unwrap();
        assert_eq!(name1, name2, "same URL must produce the same BFB name");
        assert!(name1.starts_with("bf-bundle-"));
    }

    #[tokio::test]
    async fn test_create_bfb_name_valid_k8s() {
        let url = "http://example.com/UPPER_case/special?chars=true&foo=bar#fragment";
        let name = create_bfb(&BfbMock::default(), TEST_NAMESPACE, url)
            .await
            .unwrap();
        assert!(name.len() <= 253, "name length {} exceeds 253", name.len());
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.'),
            "name contains invalid characters: {name}"
        );
        assert!(
            name.chars().next().unwrap().is_ascii_alphanumeric(),
            "name must start with alphanumeric: {name}"
        );
        assert!(
            name.chars().last().unwrap().is_ascii_alphanumeric(),
            "name must end with alphanumeric: {name}"
        );
    }

    #[tokio::test]
    async fn test_create_bfb_different_urls_different_names() {
        let mock = BfbMock::default();
        let name_a = create_bfb(&mock, TEST_NAMESPACE, "http://a.example.com/a.bfb")
            .await
            .unwrap();
        let name_b = create_bfb(&mock, TEST_NAMESPACE, "http://b.example.com/b.bfb")
            .await
            .unwrap();
        assert_ne!(name_a, name_b);
    }

    #[tokio::test]
    async fn test_create_bfb_reuses_existing() {
        let mock = BfbMock::default();
        let url = "http://example.com/reuse.bfb";
        let name1 = create_bfb(&mock, TEST_NAMESPACE, url).await.unwrap();
        let name2 = create_bfb(&mock, TEST_NAMESPACE, url).await.unwrap();
        assert_eq!(name1, name2);
        assert_eq!(
            mock.bfbs.read().unwrap().len(),
            1,
            "only one BFB should exist"
        );
    }

    /// `verify_node_labels` against a `TestLabeler` (which requires the single
    /// label `test/node=true`): a node carries the current labels only when its
    /// `metadata.labels` is a superset of the labeler's `node_labels()`. A
    /// missing node verifies as `true` because it will be (re)created with the
    /// current labels. Each row seeds one node state and asserts the verdict.
    ///
    /// Folds the six former `test_verify_node_labels_*` cases.
    #[tokio::test]
    async fn verify_node_labels_against_seeded_node() {
        use carbide_test_support::Outcome::Yields;
        use carbide_test_support::{Case, check_cases_async};

        /// What the mock's node store holds before the check runs.
        enum Seeded {
            /// No node at all under the queried name.
            Absent,
            /// A node created through `register_dpu_node`, so it carries
            /// whatever labels the labeler currently produces.
            RegisteredByLabeler,
            /// A node inserted directly with these `metadata.labels`
            /// (`None` means the labels field is absent entirely).
            WithLabels(Option<BTreeMap<String, String>>),
        }

        struct Row {
            /// Pre-existing node state in the mock.
            seeded: Seeded,
            /// Node name passed to `verify_node_labels`.
            query: &'static str,
        }

        // Build the per-row mock + SDK, seed the node, run the check.
        let run = |row: Row| async move {
            let mock = SdkMock::new();
            // A node seeded with explicit labels is inserted before the SDK is
            // built; `RegisteredByLabeler` is handled after the build (it needs
            // the SDK to apply the labeler); `Absent` seeds nothing.
            if let Seeded::WithLabels(labels) = &row.seeded {
                let node = DPUNode {
                    metadata: ObjectMeta {
                        name: Some("node-host-001".to_string()),
                        namespace: Some(TEST_NAMESPACE.to_string()),
                        labels: labels.clone(),
                        ..Default::default()
                    },
                    spec: DpuNodeSpec {
                        dpus: Some(vec![]),
                        node_dms_address: None,
                        node_reboot_method: None,
                    },
                    status: None,
                };
                mock.nodes
                    .write()
                    .unwrap()
                    .insert(SdkMock::key(&node), node);
            }

            let sdk = DpfSdkBuilder::new(mock, TEST_NAMESPACE, String::new())
                .with_labeler(TestLabeler)
                .build_without_resources()
                .await
                .unwrap();

            if matches!(row.seeded, Seeded::RegisteredByLabeler) {
                sdk.register_dpu_node(DpuNodeInfo {
                    node_id: "host-001".to_string(),
                    host_bmc_ip: "10.0.0.1".parse().unwrap(),
                    device_ids: vec!["dpu-001".to_string()],
                    deployment_type: DpuDeploymentType::Bf3,
                })
                .await
                .unwrap();
            }

            // DpfError isn't PartialEq, so render it to a String for the
            // table's Outcome comparison; these rows all expect success anyway.
            sdk.verify_node_labels(row.query, DpuDeploymentType::Bf3)
                .await
                .map_err(|e| e.to_string())
        };

        check_cases_async(
            [
                Case {
                    scenario: "node registered by labeler has current labels",
                    input: Row {
                        seeded: Seeded::RegisteredByLabeler,
                        query: "node-host-001",
                    },
                    expect: Yields(true),
                },
                Case {
                    scenario: "missing node verifies true (created with current labels)",
                    input: Row {
                        seeded: Seeded::Absent,
                        query: "node-does-not-exist",
                    },
                    expect: Yields(true),
                },
                Case {
                    scenario: "stale labels (none of the required keys) -> false",
                    input: Row {
                        seeded: Seeded::WithLabels(Some(BTreeMap::from([(
                            "old/stale-label".to_string(),
                            "true".to_string(),
                        )]))),
                        query: "node-host-001",
                    },
                    expect: Yields(false),
                },
                Case {
                    scenario: "no labels field at all -> false",
                    input: Row {
                        seeded: Seeded::WithLabels(None),
                        query: "node-host-001",
                    },
                    expect: Yields(false),
                },
                Case {
                    scenario: "superset of required labels -> true",
                    input: Row {
                        seeded: Seeded::WithLabels(Some(BTreeMap::from([
                            ("test/node".to_string(), "true".to_string()),
                            ("extra/label".to_string(), "extra-value".to_string()),
                        ]))),
                        query: "node-host-001",
                    },
                    expect: Yields(true),
                },
                Case {
                    scenario: "required key present but wrong value -> false",
                    input: Row {
                        seeded: Seeded::WithLabels(Some(BTreeMap::from([(
                            "test/node".to_string(),
                            "false".to_string(),
                        )]))),
                        query: "node-host-001",
                    },
                    expect: Yields(false),
                },
            ],
            run,
        )
        .await;
    }

    const TEST_FLAVOR: &str = "test-flavor";

    fn deployment_with(source: DpuProvisioningSource, flavor: &str) -> DPUDeployment {
        build_deployment(
            &[],
            "test-deployment",
            &source,
            flavor,
            TEST_NAMESPACE,
            &[],
            BTreeMap::new(),
            DpuDeploymentType::Bf3,
        )
    }

    /// Build a DPU through serde so the literal only names the fields each test
    /// cares about; every other CRD field defaults, and adding one upstream does
    /// not churn these tests.
    fn dpu_with(
        bfb: Option<&str>,
        blue_field_software: Option<&str>,
        flavor: &str,
        installed_bfb_file: Option<&str>,
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
            metadata: kube::core::ObjectMeta {
                name: Some("node-host-001-device-001".to_string()),
                namespace: Some(TEST_NAMESPACE.to_string()),
                ..Default::default()
            },
            spec: serde_json::from_value(spec).expect("valid DpuSpec"),
            status: Some(serde_json::from_value(status).expect("valid DpuStatus")),
        }
    }

    #[test]
    fn bfb_dpu_matching_its_deployment_is_not_outdated() {
        let deployment = deployment_with(
            DpuProvisioningSource::Bfb("bf-bundle-abc".to_string()),
            TEST_FLAVOR,
        );
        let dpu = dpu_with(
            Some("bf-bundle-abc"),
            None,
            TEST_FLAVOR,
            Some("/bfb/test-namespace-bf-bundle-abc.bfb"),
        );

        assert!(dpu_mismatch(TEST_NAMESPACE, &dpu, &deployment).is_none());
    }

    #[test]
    fn bfb_dpu_running_an_older_image_is_outdated() {
        let deployment = deployment_with(
            DpuProvisioningSource::Bfb("bf-bundle-new".to_string()),
            TEST_FLAVOR,
        );
        let dpu = dpu_with(
            Some("bf-bundle-old"),
            None,
            TEST_FLAVOR,
            Some("/bfb/test-namespace-bf-bundle-old.bfb"),
        );

        let mismatch = dpu_mismatch(TEST_NAMESPACE, &dpu, &deployment).expect("outdated");
        assert_eq!(mismatch.target_source, "test-namespace-bf-bundle-new.bfb");
    }

    #[test]
    fn blue_field_software_dpu_matching_its_deployment_is_not_outdated() {
        let deployment = deployment_with(
            DpuProvisioningSource::BlueFieldSoftware("bf-software-abc".to_string()),
            TEST_FLAVOR,
        );
        let dpu = dpu_with(None, Some("bf-software-abc"), TEST_FLAVOR, None);

        assert!(dpu_mismatch(TEST_NAMESPACE, &dpu, &deployment).is_none());
    }

    /// A BlueFieldSoftware change used to be invisible: with no BFB to compare,
    /// only the flavor was checked, so a BF4 DPU pinned to superseded software
    /// was reported as up to date and never reprovisioned.
    #[test]
    fn blue_field_software_change_marks_dpu_outdated() {
        let deployment = deployment_with(
            DpuProvisioningSource::BlueFieldSoftware("bf-software-new".to_string()),
            TEST_FLAVOR,
        );
        let dpu = dpu_with(None, Some("bf-software-old"), TEST_FLAVOR, None);

        let mismatch = dpu_mismatch(TEST_NAMESPACE, &dpu, &deployment).expect("outdated");
        assert_eq!(mismatch.target_source, "bf-software-new");
    }

    #[test]
    fn flavor_change_marks_blue_field_software_dpu_outdated() {
        let deployment = deployment_with(
            DpuProvisioningSource::BlueFieldSoftware("bf-software-abc".to_string()),
            "new-flavor",
        );
        let dpu = dpu_with(None, Some("bf-software-abc"), TEST_FLAVOR, None);

        let mismatch = dpu_mismatch(TEST_NAMESPACE, &dpu, &deployment).expect("outdated");
        assert_eq!(mismatch.target_source, "bf-software-abc");
    }

    /// The DPU CRD requires exactly one provisioning source. A deployment that
    /// satisfies neither side of that rule must not reprovision the fleet.
    #[test]
    fn deployment_with_both_or_neither_source_is_skipped() {
        let dpu = dpu_with(Some("bf-bundle-abc"), None, TEST_FLAVOR, None);

        let mut both = deployment_with(
            DpuProvisioningSource::Bfb("bf-bundle-abc".to_string()),
            TEST_FLAVOR,
        );
        both.spec.dpus.blue_field_software = Some("bf-software-abc".to_string());
        assert!(dpu_mismatch(TEST_NAMESPACE, &dpu, &both).is_none());

        let mut neither = both;
        neither.spec.dpus.bfb = None;
        neither.spec.dpus.blue_field_software = None;
        assert!(dpu_mismatch(TEST_NAMESPACE, &dpu, &neither).is_none());
    }
}

#[cfg(test)]
mod configmap_seed_tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::repository::K8sConfigRepository;

    const NS: &str = "dpf-operator-system";

    /// Records applies and lets a test seed already-existing ConfigMaps.
    #[derive(Default)]
    struct ConfigMapMock {
        existing: Mutex<BTreeMap<String, BTreeMap<String, String>>>,
        applied: Mutex<Vec<(String, BTreeMap<String, String>)>>,
    }

    impl ConfigMapMock {
        fn seeded(name: &str, data: BTreeMap<String, String>) -> Self {
            let mock = Self::default();
            mock.existing.lock().unwrap().insert(name.to_string(), data);
            mock
        }

        fn applied_names(&self) -> Vec<String> {
            self.applied
                .lock()
                .unwrap()
                .iter()
                .map(|(name, _)| name.clone())
                .collect()
        }
    }

    #[async_trait]
    impl K8sConfigRepository for ConfigMapMock {
        async fn get_configmap(
            &self,
            name: &str,
            _ns: &str,
        ) -> Result<Option<BTreeMap<String, String>>, DpfError> {
            Ok(self.existing.lock().unwrap().get(name).cloned())
        }
        async fn create_configmap(
            &self,
            name: &str,
            _ns: &str,
            data: BTreeMap<String, String>,
        ) -> Result<bool, DpfError> {
            let mut existing = self.existing.lock().unwrap();
            if existing.contains_key(name) {
                return Ok(false);
            }
            self.applied
                .lock()
                .unwrap()
                .push((name.to_string(), data.clone()));
            existing.insert(name.to_string(), data);
            Ok(true)
        }

        async fn apply_configmap(
            &self,
            _name: &str,
            _ns: &str,
            _data: BTreeMap<String, String>,
        ) -> Result<(), DpfError> {
            unreachable!("seeding must never apply; it is create-only")
        }
        async fn get_secret(
            &self,
            _name: &str,
            _ns: &str,
        ) -> Result<Option<BTreeMap<String, Vec<u8>>>, DpfError> {
            Ok(None)
        }
        async fn apply_secret(
            &self,
            _name: &str,
            _ns: &str,
            _data: BTreeMap<String, Vec<u8>>,
        ) -> Result<(), DpfError> {
            Ok(())
        }
    }

    /// BF3 does not require the Astra Spectrum-X runtime ConfigMap.
    #[tokio::test]
    async fn bf3_does_not_require_the_astra_runtime_configmap() {
        let mock = ConfigMapMock::default();
        create_extra_script_configmaps(&mock, NS, DpuDeploymentType::Bf3)
            .await
            .expect("seeding succeeds");
        validate_bf4_astra_ra2_2_runtime_configmap(&mock, NS, DpuDeploymentType::Bf3)
            .await
            .expect("BF3 does not require the Astra runtime ConfigMap");
        assert!(mock.applied_names().is_empty());
    }

    /// Astra initialization fails rather than creating a placeholder Spectrum-X configuration.
    #[tokio::test]
    async fn bf4_astra_requires_the_runtime_configmap() {
        let mock = ConfigMapMock::default();
        let error =
            validate_bf4_astra_ra2_2_runtime_configmap(&mock, NS, DpuDeploymentType::Bf4Astra)
                .await
                .expect_err("Astra must require the site-owned runtime ConfigMap");

        assert!(matches!(
            error,
            DpfError::ConfigError(message)
                if message.contains("dpf-operator-system/ra2.2-runtime")
                    && message.contains("RA2.2-runtime.yaml")
        ));
        assert!(mock.applied_names().is_empty());
    }

    /// A site-owned runtime configuration must be the one the flavor consumes.
    #[tokio::test]
    async fn existing_bf4_astra_runtime_configmap_is_left_alone() {
        let site_config = "runtimeConfig:\n  roce: []\n";
        let mock = ConfigMapMock::seeded(
            BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_NAME,
            BTreeMap::from([(
                BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_KEY.to_string(),
                site_config.to_string(),
            )]),
        );

        validate_bf4_astra_ra2_2_runtime_configmap(&mock, NS, DpuDeploymentType::Bf4Astra)
            .await
            .expect("existing site configuration is valid");

        assert!(mock.applied_names().is_empty());
        assert_eq!(
            mock.existing.lock().unwrap()[BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_NAME]
                [BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_KEY],
            site_config
        );
    }

    #[tokio::test]
    async fn bf4_astra_runtime_configmap_requires_the_referenced_key() {
        let mock = ConfigMapMock::seeded(BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_NAME, BTreeMap::new());

        let error =
            validate_bf4_astra_ra2_2_runtime_configmap(&mock, NS, DpuDeploymentType::Bf4Astra)
                .await
                .expect_err("Astra runtime ConfigMap must contain the referenced key");

        assert!(matches!(
            error,
            DpfError::ConfigError(message) if message.contains(BF4_ASTRA_RA2_2_RUNTIME_CONFIGMAP_KEY)
        ));
    }

    /// Names and key must stay in lockstep with the flavors' `configMapKeyRef`.
    #[tokio::test]
    async fn bf4_seeds_both_hooks_under_the_referenced_key() {
        for (deployment_type, expected) in [
            (
                DpuDeploymentType::Bf4Generic,
                [
                    "extra-script-pre-ovs-bf4-generic",
                    "extra-script-post-ovs-bf4-generic",
                ],
            ),
            (
                DpuDeploymentType::Bf4Astra,
                [
                    "extra-script-pre-ovs-bf4-astra",
                    "extra-script-post-ovs-bf4-astra",
                ],
            ),
        ] {
            let mock = ConfigMapMock::default();
            create_extra_script_configmaps(&mock, NS, deployment_type)
                .await
                .expect("seeding succeeds");

            assert_eq!(mock.applied_names(), expected, "{deployment_type:?}");
            for (_, data) in mock.applied.lock().unwrap().iter() {
                let script = data
                    .get(EXTRA_SCRIPT_CONFIGMAP_KEY)
                    .expect("script key is present");
                assert!(
                    script.starts_with("#!"),
                    "placeholder needs a shebang, the flavor runs it by path: {script:?}"
                );
            }
        }
    }

    /// Re-running initialization must not put the placeholder back over an edit.
    #[tokio::test]
    async fn an_operator_edited_script_is_left_alone() {
        let operator_script = "#!/usr/bin/env bash\necho site-specific\n";
        let mock = ConfigMapMock::seeded(
            "extra-script-pre-ovs-bf4-generic",
            BTreeMap::from([(
                EXTRA_SCRIPT_CONFIGMAP_KEY.to_string(),
                operator_script.to_string(),
            )]),
        );

        create_extra_script_configmaps(&mock, NS, DpuDeploymentType::Bf4Generic)
            .await
            .expect("seeding succeeds");

        assert_eq!(
            mock.applied_names(),
            vec!["extra-script-post-ovs-bf4-generic"],
            "only the absent hook is created"
        );
        assert_eq!(
            mock.existing.lock().unwrap()["extra-script-pre-ovs-bf4-generic"]
                [EXTRA_SCRIPT_CONFIGMAP_KEY],
            operator_script
        );
    }

    /// Seeding is create-only, so an existing ConfigMap is never written to,
    /// whatever it holds. NICo has no `update` on these at the RBAC layer either.
    #[tokio::test]
    async fn an_existing_configmap_is_never_written_to() {
        for seeded in [
            BTreeMap::new(),
            BTreeMap::from([("other".into(), "x".into())]),
        ] {
            let mock = ConfigMapMock::seeded("extra-script-pre-ovs-bf4-astra", seeded.clone());
            create_extra_script_configmaps(&mock, NS, DpuDeploymentType::Bf4Astra)
                .await
                .expect("seeding succeeds");

            assert_eq!(
                mock.applied_names(),
                vec!["extra-script-post-ovs-bf4-astra"],
                "only the absent ConfigMap is created"
            );
            assert_eq!(
                mock.existing.lock().unwrap()["extra-script-pre-ovs-bf4-astra"],
                seeded,
                "the existing ConfigMap is left byte-for-byte alone"
            );
        }
    }
}
