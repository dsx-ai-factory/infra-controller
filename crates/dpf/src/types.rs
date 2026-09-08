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

//! SDK types for the DPF SDK.

use std::collections::BTreeMap;
use std::net::IpAddr;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use serde::{Deserialize, Serialize};

use crate::crds::dpus_generated::{
    DpuStatusAgentStatus, DpuStatusOperationalConditions, DpuStatusPhase,
};

/// Async provider for BMC passwords used to create and refresh the K8s BMC
/// secret. Implement this trait to supply credentials dynamically (e.g. from
/// a vault or credential manager).
#[async_trait::async_trait]
pub trait BmcPasswordProvider: Send + Sync {
    async fn get_bmc_password(&self) -> Result<String, crate::DpfError>;
}

#[async_trait::async_trait]
impl BmcPasswordProvider for String {
    async fn get_bmc_password(&self) -> Result<String, crate::DpfError> {
        Ok(self.clone())
    }
}

/// Service name constants for use across crates
pub const DOCA_HBN_SERVICE_NAME: &str = "doca-hbn";
pub const DHCP_SERVER_SERVICE_NAME: &str = "carbide-dhcp-server";
/// Shared marker applied to every DPF-managed DPUNode.
pub const DPU_ENABLED_NODE_LABEL: &str = "feature.node.kubernetes.io/dpu-enabled";
pub const FMDS_SERVICE_NAME: &str = "carbide-fmds";
pub const DPU_AGENT_SERVICE_NAME: &str = "carbide-dpu-agent";
pub const OTEL_COLLECTOR_SERVICE_NAME: &str = "carbide-otelcol";
pub const DTS_SERVICE_NAME: &str = "dts";
pub const DOCA_WEAVE_DHCP_AGENT_SERVICE_NAME: &str = "doca-weave-dhcp-agent";
/// Number of PF scalable functions (SFs) allocated to the DOCA Weave DHCP Agent.
pub const DOCA_WEAVE_DHCP_AGENT_PF_TOTAL_SF: u32 = 8;
/// Additional PF scalable functions (SFs) reserved for Astra capacity headroom.
pub const PF_TOTAL_SF_BF4_ASTRA_FUDGE: u32 = 4;
pub const DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_NAME: &str = "doca-weave-flow-controller";
pub const DOCA_XPLANE_SERVICE_NAME: &str = "doca-xplane";
/// Hash-stable legacy VF population used by default DPF flavors and SDK initialization.
pub const DEFAULT_DPU_NUM_OF_VFS: u32 = 16;
/// Default SF capacity reserved beyond configured NICo-managed service endpoints.
pub const DEFAULT_PF_TOTAL_SF_RESERVED: u32 = 30;
// Keep direct SDK validation aligned with api-core's general BlueField provisioning bound without
// coupling this lightweight crate to the complete API configuration model.
pub(crate) const MAX_BLUEFIELD_VFS_PER_PF: u32 = 126;
// Keep topology validation aligned with `model::instance::config::network::INTERFACE_VFID_MAX`
// without adding the complete API model as a DPF SDK dependency.
const MAX_INSTANCE_VF_ID: u8 = 15;

/// Configuration for creating DPF operator resources (BFB or
/// BlueFieldSoftware, DPUFlavor, DPUDeployment, services, etc.).
#[derive(Debug, Clone)]
pub struct InitDpfResourcesConfig {
    /// URL for the BFB (BlueField Bundle) image. Used for BF3-class DPUs.
    /// Ignored when [`bluefield_software`](Self::bluefield_software) is set.
    pub bfb_url: String,
    /// BlueFieldSoftware spec for BF4-class DPUs. When set, a `BlueFieldSoftware`
    /// CR is created and referenced by the DPUDeployment instead of a BFB, and
    /// [`bfb_url`](Self::bfb_url) is ignored. Exactly one provisioning source
    /// (BFB or BlueFieldSoftware) is expected per deployment.
    pub bluefield_software: Option<BlueFieldSoftwareParams>,
    /// Name of the DPUDeployment CR.
    pub deployment_name: String,
    /// Name of the DPUFlavor CR.
    pub flavor_name: String,
    /// Service templates and configs for M4 DPUDeployment.
    /// When empty, `default_services()` is used automatically.
    pub services: Vec<ServiceDefinition>,

    /// Number of hardware VFs provisioned per DPU PF for BF3 and generic BF4.
    pub num_of_vfs: u32,
    /// SF capacity reserved beyond configured NICo-managed service endpoints.
    /// Without intercept bridging, this remains the complete legacy `PF_TOTAL_SF` value.
    pub pf_total_sf_reserved: u32,
    /// Managed SF capacity not represented in [`interfaces`](Self::interfaces).
    /// Added to calculated BF3/generic-BF4 capacity when intercept bridging is configured;
    /// otherwise it consumes the unchanged legacy pool. BF4 Astra rejects a non-zero value.
    pub additional_managed_sf: u32,
    /// Enables deployment-scoped DPUServiceInterface names and node selectors.
    /// False preserves the legacy global resource naming and selector mode for
    /// BF3 and generic BF4; BF4 Astra requires this to be true.
    /// Mode transitions require manual old-resource cleanup and DPU re-ingestion;
    /// the SDK neither detects nor deletes the previous generation.
    pub deployment_scoped_service_interfaces: bool,
    /// Optional intercept-bridging topology for BF3 and generic BF4. `Some` replaces the
    /// ordinary static PF/VF inventory and contains exactly one configured PF.
    pub intercept_bridging: Option<DpfInterceptBridging>,
    /// Effective interface inventory shared by ServiceInterfaces, service chains,
    /// and caller-built service definitions. Empty asks the SDK to build it. With
    /// intercept bridging, a non-empty inventory must exactly match the SDK projection.
    pub interfaces: Vec<DpuServiceInterfaceTemplateDefinition>,

    pub proxy: Option<DpfProxyDetails>,
    /// Operator-supplied bf.cfg lines appended to the flavor's built-in `bfcfgParameters`.
    ///
    /// Passed through verbatim; the SDK applies no quoting or interpretation. Entries containing
    /// the Go template delimiter `{{` are rejected, since BF4 Astra renders its
    /// DPUFlavorTemplate body and could not pass them through.
    ///
    /// WARNING: Changing this will generate a new DPUFlavor, reprovisioning the deployment's
    /// DPUs.
    pub extra_bfcfg_parameters: Vec<String>,
    /// Deployment type — determines which DPUFlavor spec to build.
    pub deployment_type: DpuDeploymentType,
}

/// Parameters for a `BlueFieldSoftware` CR, used to provision BF4-class DPUs.
/// Mirrors the `spec` of the `provisioning.dpu.nvidia.com/v1alpha1`
/// `BlueFieldSoftware` resource.
#[derive(Debug, Clone)]
pub struct BlueFieldSoftwareParams {
    /// OS ISO URL used by the DPU OS installation flow (`spec.osIso`).
    pub os_iso: String,
    /// Optional PLDM firmware bundle URL for baseline firmware updates
    /// (`spec.pldmFwBundle`).
    pub pldm_fw_bundle: Option<String>,
}

impl Default for InitDpfResourcesConfig {
    fn default() -> Self {
        Self {
            bfb_url: String::new(),
            bluefield_software: None,
            deployment_name: "dpu-deployment".to_string(),
            flavor_name: crate::flavor::DEFAULT_FLAVOR_NAME.to_string(),
            services: Vec::new(),
            num_of_vfs: DEFAULT_DPU_NUM_OF_VFS,
            pf_total_sf_reserved: DEFAULT_PF_TOTAL_SF_RESERVED,
            additional_managed_sf: 0,
            deployment_scoped_service_interfaces: false,
            intercept_bridging: None,
            interfaces: Vec::new(),
            proxy: None,
            extra_bfcfg_parameters: Vec::new(),
            deployment_type: DpuDeploymentType::Bf3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DpfProxyDetails {
    pub https_proxy: String,
    #[serde(default)]
    pub no_proxy: Vec<String>,
}

/// A DPU CR whose installed BFB, BlueFieldSoftware, or `spec.dpuFlavor` does
/// not match the expected one. Returned by
/// [`crate::DpfSdk::find_outdated_dpus_dpf`]; the labels map is the DPU CR's
/// `metadata.labels` so callers can map back to their own identifiers.
#[derive(Debug, Clone)]
pub struct DpuMismatch {
    pub dpu_cr_name: String,
    pub dpu_labels: std::collections::BTreeMap<String, String>,
    /// Expected provisioning source, for traceability only. For a BFB-based
    /// deployment this is the expected BFB filename (e.g.
    /// `<namespace>-bf-bundle-<sha256>.bfb`); for a BlueFieldSoftware-based one
    /// it is the expected BlueFieldSoftware CR name.
    pub target_source: String,
}

/// Service type for configPorts (DPUServiceConfiguration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPortsServiceType {
    NodePort,
    ClusterIp,
    None,
}

/// Single port entry for DPUServiceConfiguration.serviceConfiguration.configPorts.
#[derive(Debug, Clone)]
pub struct ServiceConfigPort {
    pub name: String,
    pub port: i64,
    pub protocol: ServiceConfigPortProtocol,
    pub node_port: Option<i64>,
}

/// Resource kind allocated by a service Network Attachment Definition (NAD).
#[derive(Debug, Clone)]
pub enum ServiceNADResourceType {
    Vf,
    Sf,
    Veth,
}

#[derive(Debug, Clone)]
pub struct ServiceNAD {
    pub name: String,
    pub bridge: Option<String>,
    pub ipam: Option<bool>,
    pub resource_type: ServiceNADResourceType,
    pub mtu: Option<i64>,
}

/// Protocol for a config port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceConfigPortProtocol {
    Tcp,
    Udp,
}

/// Definition of a DPU service (DPUServiceTemplate + DPUServiceConfiguration).
#[derive(Debug, Clone, Default)]
pub struct ServiceDefinition {
    /// Service name (e.g. "dts").
    pub name: String,
    /// Helm chart repository URL.
    pub helm_repo_url: String,
    /// Helm chart name.
    pub helm_chart: String,
    /// Helm chart version.
    pub helm_version: String,
    /// Optional helm values for the template (merged into chart).
    pub helm_values: Option<serde_json::Value>,
    /// Network interfaces for the service.
    pub interfaces: Vec<ServiceInterface>,
    /// Optional service configuration (helm values for DPUServiceConfiguration).
    pub config_values: Option<serde_json::Value>,
    /// Config ports for DPUServiceConfiguration (e.g. DTS httpserverport 9189).
    pub config_ports: Option<Vec<ServiceConfigPort>>,
    /// Service type for config_ports (e.g. None for DTS).
    pub config_ports_service_type: Option<ConfigPortsServiceType>,
    /// Service chain switches connecting physical interfaces to this service's interfaces.
    pub service_chain_switches: Vec<ServiceChainSwitch>,
    /// Optional annotations for the service DaemonSet (e.g. Multus CNI networks).
    pub service_daemon_set_annotations: Option<std::collections::BTreeMap<String, String>>,
    /// Optional extended resources requested by the service DaemonSet.
    pub service_daemon_set_resources: Option<BTreeMap<String, IntOrString>>,
    /// Optional service Network Attachment Definition specification
    pub service_nad: Option<ServiceNAD>,
}

/// Interface kind rendered into a DPUServiceInterface template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DpuServiceInterfaceTemplateType {
    Physical,
    Pf,
    Vf,
    Patch(DpuServiceInterfacePatch),
}

/// Typed identity of a PF or VF selected for DPF intercept bridging topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DpfInterfaceIdentity {
    pub controller_id: u8,
    pub pf_id: u8,
    pub vf_id: Option<u8>,
}

impl DpfInterfaceIdentity {
    /// Returns the deterministic DPF resource name for this identity.
    pub fn resource_name(self) -> String {
        match self.vf_id {
            Some(vf_id) => format!("c{}pf{}vf{vf_id}", self.controller_id, self.pf_id),
            None => format!("c{}pf{}", self.controller_id, self.pf_id),
        }
    }

    /// Returns the canonical service-local endpoint stem.
    ///
    /// Configured topology may select any hardware controller/PF, but NICo's
    /// HBN, DHCP, and FMDS runtime contract supports one selected PF. Hardware
    /// identity remains in DPF resource names and flavor topology; service
    /// containers consistently expose that selected parent as logical PF0.
    pub fn service_interface_stem(self) -> String {
        match self.vf_id {
            Some(vf_id) => format!("pf0vf{vf_id}"),
            None => "pf0hpf".to_string(),
        }
    }

    /// Returns the semantic generic-BF4 PF identity exposed by `phys_port_name`.
    pub fn bf4_phys_port_name(self) -> String {
        format!("c{}pf{}", self.controller_id, self.pf_id)
    }

    /// Returns the BF3 raw representor name selected by this identity.
    pub fn bf3_raw_netdev_name(self) -> String {
        match self.vf_id {
            Some(vf_id) => format!("pf{}vf{vf_id}", self.pf_id),
            None => format!("pf{}hpf", self.pf_id),
        }
    }
}

/// One normalized intercept-bridging interface consumed by DPF resource generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpfInterceptBridge {
    pub(crate) identity: DpfInterfaceIdentity,
    pub(crate) bridge: String,
    pub(crate) patch_port: String,
}

impl DpfInterceptBridge {
    /// Builds one normalized entry without retaining its legacy configuration key.
    pub fn new(
        identity: DpfInterfaceIdentity,
        bridge: impl Into<String>,
        patch_port: impl Into<String>,
    ) -> Self {
        Self {
            identity,
            bridge: bridge.into(),
            patch_port: patch_port.into(),
        }
    }
}

/// Deterministic, validated intercept-bridging topology shared by BF3 and generic BF4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpfInterceptBridging {
    interfaces: Vec<DpfInterceptBridge>,
    num_of_vfs: u32,
}

impl DpfInterceptBridging {
    /// Requires one PF, validates configured identities and rendered names, then sorts by identity.
    pub fn new(
        mut interfaces: Vec<DpfInterceptBridge>,
        num_of_vfs: u32,
    ) -> Result<Self, crate::DpfError> {
        if num_of_vfs > MAX_BLUEFIELD_VFS_PER_PF {
            return Err(crate::DpfError::ConfigError(format!(
                "DPF num_of_vfs must be <= {MAX_BLUEFIELD_VFS_PER_PF}"
            )));
        }

        // Canonical order makes every downstream resource and flavor hash deterministic.
        interfaces.sort_by_key(|interface| interface.identity);

        // FMDS serves one logical PF endpoint, so every configured replacement inventory must
        // include the selected PF. Existing validation then guarantees that PF is unique.
        if !interfaces
            .iter()
            .any(|interface| interface.identity.vf_id.is_none())
        {
            return Err(crate::DpfError::ConfigError(
                "DPF intercept bridging requires exactly one configured PF interface".to_string(),
            ));
        }

        // Track generated names in each namespace while validating every configured entry.
        let mut identities = std::collections::BTreeSet::new();
        let mut resource_names = std::collections::BTreeSet::new();
        let mut hbn_interface_names = std::collections::BTreeSet::new();
        let mut dhcp_interface_names = std::collections::BTreeSet::new();
        let mut fmds_interface_names = std::collections::BTreeSet::new();
        // Seed OVS names created independently of configured topology. Operator-provided bridge
        // and patch names share this OVS namespace and must not reuse any of them.
        let mut ovs_names = BTreeMap::from([
            ("br-sfc".to_string(), "DPF service-chain bridge".to_string()),
            ("br-hbn".to_string(), "DPF HBN bridge".to_string()),
            ("p0".to_string(), "fixed physical interface".to_string()),
            ("p1".to_string(), "fixed physical interface".to_string()),
        ]);

        let selected_parents = interfaces
            .iter()
            .map(|interface| (interface.identity.controller_id, interface.identity.pf_id))
            .collect::<std::collections::BTreeSet<_>>();

        // Runtime service configuration addresses one logical PF. The typed identity still
        // selects which hardware controller/PF becomes that logical PF0.
        if selected_parents.len() > 1 {
            return Err(crate::DpfError::ConfigError(
                "DPF intercept bridging supports one selected controller/PF parent".to_string(),
            ));
        }
        let &(_, selected_pf_id) = selected_parents
            .first()
            .expect("the required PF interface supplies one selected parent");

        for interface in &interfaces {
            let identity = interface.identity;
            let resource_name = identity.resource_name();

            // A duplicate identity would otherwise render the same Kubernetes and service names.
            if !identities.insert(identity) {
                return Err(crate::DpfError::ConfigError(format!(
                    "duplicate DPF intercept-bridging interface identity {resource_name}"
                )));
            }

            // PF selections are independent of the provisioned VF population. Topology VFs are
            // exclusively VM instance endpoints, so their identity is bounded by both hardware
            // provisioning and the instance network model.
            if let Some(vf_id) = identity.vf_id {
                if vf_id > MAX_INSTANCE_VF_ID {
                    return Err(crate::DpfError::ConfigError(format!(
                        "DPF intercept-bridging interface {resource_name} selects VF {vf_id}, but instance networking supports at most VF{MAX_INSTANCE_VF_ID}"
                    )));
                }
                if u32::from(vf_id) >= num_of_vfs {
                    return Err(crate::DpfError::ConfigError(format!(
                        "DPF intercept-bridging interface {resource_name} selects a VF outside num_of_vfs={num_of_vfs}"
                    )));
                }
            }

            validate_linux_netdev_name(&interface.bridge, "bridge", &resource_name)?;
            validate_ovs_patch_name(&interface.patch_port, &resource_name)?;

            // Resource names are derived from typed numeric identity, but retain an explicit
            // collision gate so future naming changes cannot silently alias resources.
            if !resource_names.insert(resource_name.clone()) {
                return Err(crate::DpfError::ConfigError(format!(
                    "generated DPF intercept-bridging resource name {resource_name} is not unique"
                )));
            }

            // Runtime services expose the selected hardware parent as logical PF0. Validate the
            // generated Linux endpoint names separately per service: the same stem may appear in
            // different service namespaces, but every name within one service must fit IFNAMSIZ
            // and remain unique.
            let service_interface_stem = identity.service_interface_stem();
            for (name, names) in [
                (
                    format!("{service_interface_stem}_if"),
                    &mut hbn_interface_names,
                ),
                (
                    format!("d_{service_interface_stem}_if"),
                    &mut dhcp_interface_names,
                ),
            ] {
                if name.len() > 15 || !names.insert(name.clone()) {
                    return Err(crate::DpfError::ConfigError(format!(
                        "generated DPF service interface name {name} is invalid or not unique"
                    )));
                }
            }
            if identity.vf_id.is_none() {
                // Only the selected PF supplies FMDS, so its endpoint has a separate namespace.
                let fmds_name = format!("f_{service_interface_stem}_if");
                if fmds_name.len() > 15 || !fmds_interface_names.insert(fmds_name.clone()) {
                    return Err(crate::DpfError::ConfigError(format!(
                        "generated DPF service interface name {fmds_name} is invalid or not unique"
                    )));
                }
            }

            // DPF creates the local br-sfc side of each Patch pair with this deterministic name.
            // It is an OVS object even though operators configure only the peer-side name, so it
            // must participate in the same collision gate as every explicit bridge and port.
            let local_patch_port = format!("p_brsfc_to_{}", interface.patch_port);
            for (name, purpose) in [
                (&interface.bridge, "intermediate bridge"),
                (&interface.patch_port, "peer patch port"),
                (&local_patch_port, "DPF-generated local patch port"),
            ] {
                if let Some(existing) = ovs_names.insert(
                    name.clone(),
                    format!("{purpose} for DPF intercept-bridging interface {resource_name}"),
                ) {
                    return Err(crate::DpfError::ConfigError(format!(
                        "OVS name {name} for {purpose} on DPF intercept-bridging interface {resource_name} conflicts with {existing}"
                    )));
                }
            }
        }

        // Configured topology replaces the static ServiceInterface inventory, but BF3 still
        // provisions the complete NUM_OF_VFS hardware population. For example, topology may select
        // PF2 and omit VF4 while hardware still creates `pf2vf4`; accepting that name for a bridge
        // or patch port would collide when the representor materializes. Reserve every provisioned
        // raw representor because all of them share OVS's global object namespace.
        if let Some((raw_netdev, existing)) = ovs_names
            .iter()
            .find(|(name, _)| is_provisioned_bf3_raw_netdev(name, selected_pf_id, num_of_vfs))
        {
            return Err(crate::DpfError::ConfigError(format!(
                "expected provisioned BF3 raw netdev {raw_netdev} conflicts with {existing}"
            )));
        }

        Ok(Self {
            interfaces,
            num_of_vfs,
        })
    }

    /// Returns the normalized entries in deterministic typed-identity order.
    pub(crate) fn interfaces(&self) -> &[DpfInterceptBridge] {
        &self.interfaces
    }

    /// Returns the VF population against which this topology was validated.
    pub(crate) fn num_of_vfs(&self) -> u32 {
        self.num_of_vfs
    }
}

/// Returns whether an OVS object name aliases a raw BF3 representor provisioned for the PF.
fn is_provisioned_bf3_raw_netdev(name: &str, pf_id: u8, num_of_vfs: u32) -> bool {
    let pf_stem = format!("pf{pf_id}");
    if name == format!("{pf_stem}hpf") {
        return true;
    }

    let vf_stem = format!("{pf_stem}vf");
    name.strip_prefix(&vf_stem)
        .and_then(|suffix| suffix.parse::<u32>().ok())
        .is_some_and(|vf_id| vf_id < num_of_vfs && name == format!("{vf_stem}{vf_id}"))
}

/// Rejects Linux netdev names outside NICo's shell-safe DPF contract.
fn validate_linux_netdev_name(
    name: &str,
    purpose: &str,
    resource_name: &str,
) -> Result<(), crate::DpfError> {
    // Intermediate bridges are Linux netdevs and must fit IFNAMSIZ.
    let valid = (1..=15).contains(&name.len())
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.bytes().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == b'-'
        });
    if !valid {
        return Err(crate::DpfError::ConfigError(format!(
            "DPF intercept bridging {purpose} name {name:?} for interface {resource_name} must be 1 to 15 lowercase ASCII letters, digits, or hyphens and start with a letter"
        )));
    }
    Ok(())
}

/// Rejects OVS patch-port names outside NICo's shell-safe DPF contract.
fn validate_ovs_patch_name(name: &str, resource_name: &str) -> Result<(), crate::DpfError> {
    // Patch ports are OVSDB objects, not Linux netdev names. The checked-in DPF CRD has no
    // maxLength and demonstrates an autogenerated name longer than 15 characters containing
    // underscores, so do not invent a Linux-netdev length restriction here.
    let valid = !name.is_empty()
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.bytes().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, b'-' | b'_')
        });
    if !valid {
        return Err(crate::DpfError::ConfigError(format!(
            "DPF intercept bridging patch port name {name:?} for interface {resource_name} must be non-empty, start with a lowercase ASCII letter, and contain only lowercase ASCII letters, digits, hyphens, or underscores"
        )));
    }
    Ok(())
}

/// Patch-pair data owned by a DPUServiceInterface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpuServiceInterfacePatch {
    pub(crate) peer_bridge: String,
    pub(crate) peer_patch_name: String,
    pub(crate) peer_external_ids: Option<BTreeMap<String, String>>,
}

/// Network interface for a DPU service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpuServiceInterfaceTemplateDefinition {
    /// Interface name.
    pub name: String,
    /// Interface Type
    pub iface_type: DpuServiceInterfaceTemplateType,
    /// PF Interface ID
    pub pf_id: i64,
    /// VF Interface ID
    pub vf_id: i64,
    /// Chained service interfaces vector
    pub chained_svc_if: Option<Vec<(String, String)>>,
}

/// Network interface for a DPU service.
#[derive(Debug, Clone)]
pub struct ServiceInterface {
    /// Interface name.
    pub name: String,
    /// Network name.
    pub network: String,
}

/// Service chain switch connecting a physical interface to a service interface.
#[derive(Debug, Clone)]
pub struct ServiceChainSwitch {
    /// Physical interface label (e.g. "p0", "p1", "pf0hpf").
    pub physical_interface: String,
    /// Service name (e.g. "doca-hbn").
    pub service_name: String,
    /// Interface name on the service (e.g. "p0_if").
    pub service_interface: String,
}

impl ServiceDefinition {
    /// Create a service definition with the required helm chart fields.
    pub fn new(
        name: impl Into<String>,
        helm_repo_url: impl Into<String>,
        helm_chart: impl Into<String>,
        helm_version: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            helm_repo_url: helm_repo_url.into(),
            helm_chart: helm_chart.into(),
            helm_version: helm_version.into(),
            ..Default::default()
        }
    }
}

/// Desired definition of a direct, detached DPUService.
///
/// This is deliberately distinct from [`ServiceDefinition`], which produces a
/// DPUServiceTemplate/DPUServiceConfiguration pair for DPUDeployment. A
/// detached service has no DPUDeployment, service ID, interfaces, config
/// ports, or DPU-cluster selector.
#[derive(Debug, Clone)]
pub struct DetachedDpuServiceDefinition {
    pub name: String,
    pub namespace: String,
    pub labels: BTreeMap<String, String>,
    pub helm_chart: DetachedHelmChart,
    pub deploy_in_cluster: bool,
    pub security_privileged: bool,
    /// Exact-match node labels for the detached service's DaemonSet. The SDK
    /// renders these as one NodeSelector term with `In` expressions.
    pub node_selector_labels: BTreeMap<String, String>,
}

/// Helm-chart fields required by a [`DetachedDpuServiceDefinition`].
#[derive(Debug, Clone)]
pub struct DetachedHelmChart {
    pub repo_url: String,
    pub chart: String,
    pub version: String,
    pub release_name: String,
    pub values: Option<BTreeMap<String, serde_json::Value>>,
}

/// SDK-owned view of any observed DPUService.
///
/// This can represent a direct service or a DPUDeployment-generated service.
/// Optional fields deliberately preserve what the API returned; callers must
/// validate ownership before treating it as a detached NICo service.
#[derive(Debug, Clone)]
pub struct DpuServiceObservation {
    pub name: Option<String>,
    pub namespace: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub helm_chart: DpuServiceHelmChartObservation,
    pub deploy_in_cluster: Option<bool>,
    pub dpu_cluster_selector_present: bool,
    pub interfaces_present: bool,
    pub paused: Option<bool>,
    pub security_privileged: Option<bool>,
    /// Serialized Kubernetes NodeSelector, retained for immutable ownership
    /// validation.
    pub service_daemon_set_node_selector: Option<serde_json::Value>,
    pub service_id: Option<String>,
    pub config_ports_present: bool,
    /// Whether Kubernetes has accepted deletion and the CR is retained only
    /// while finalizers remove its dependent resources.
    pub is_deleting: bool,
}

/// Helm-chart fields as observed on a live DPUService.
#[derive(Debug, Clone)]
pub struct DpuServiceHelmChartObservation {
    pub repo_url: String,
    pub chart: Option<String>,
    pub version: String,
    pub release_name: Option<String>,
    pub values: Option<BTreeMap<String, serde_json::Value>>,
}

/// Deployment type of a DPU — used to route devices to the correct
/// DPUDeployment and select the appropriate DPUFlavor configuration.
///
/// BF4-class DPUs are provisioned from a single `BlueFieldSoftware` CR (the CR
/// itself carries the PSID→PLDM mapping), so there is one BF4 deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DpuDeploymentType {
    Bf3,
    /// BF3 B3240 on a GB200 platform using CPU as Root Complex mode.
    Bf3Gb200,
    Bf4Generic,
    Bf4Astra,
}

/// Information about a DPU device (DPUDevice CR).
#[derive(Debug, Clone)]
pub struct DpuDeviceInfo {
    /// Identifier for this device (e.g. `01-02-03-04-05-06`).
    /// Used as the DPUDevice CR name.
    pub device_id: String,
    /// BMC IP address for the DPU.
    pub dpu_bmc_ip: IpAddr,
    /// BMC IP address for the host.
    pub host_bmc_ip: IpAddr,
    /// Serial number of the DPU.
    pub serial_number: String,
    /// Caller-defined identifier for the DPU machine.
    /// Passed through to the labeler for resource labels.
    pub dpu_machine_id: String,
    /// is _primary dpu?
    pub is_primary: bool,
}

/// Information about a DPU node (host with DPUs).
#[derive(Debug, Clone)]
pub struct DpuNodeInfo {
    /// Identifier for this node (e.g. `01-02-03-04-05-06`).
    /// Used to build the DPUNode CR name via `dpu_node_cr_name()`.
    pub node_id: String,
    /// BMC IP of the host.
    pub host_bmc_ip: IpAddr,
    /// Identifiers of each device attached to this node.
    pub device_ids: Vec<String>,
    /// Deployment type for the DPUs on this node — used to look up deployment-specific labels.
    pub deployment_type: DpuDeploymentType,
}

/// Phase of DPU lifecycle.
///
/// This is a simplified view - the DPF operator has many more internal phases,
/// but callers typically only care about these actionable states.
/// Provisioning sub-phases are represented as Provisioning(detail) so the
/// detailed phase is still visible for debugging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DpuPhase {
    /// DPU is being provisioned by the operator.
    Provisioning(String),
    /// DPU is waiting on node effect (maintenance hold).
    NodeEffect,
    /// Host reboot required before DPU can progress.
    Rebooting,
    /// DPU is ready and operational.
    Ready,
    /// DPU is in an error state.
    Error,
    /// DPU is being deleted.
    Deleting,
}

impl AsRef<str> for DpuPhase {
    fn as_ref(&self) -> &str {
        match self {
            DpuPhase::Provisioning(detail) => detail.as_str(),
            DpuPhase::NodeEffect => "NodeEffect",
            DpuPhase::Rebooting => "Rebooting",
            DpuPhase::Ready => "Ready",
            DpuPhase::Error => "Error",
            DpuPhase::Deleting => "Deleting",
        }
    }
}

impl std::fmt::Display for DpuPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}

impl From<DpuStatusPhase> for DpuPhase {
    fn from(phase: DpuStatusPhase) -> Self {
        match phase {
            DpuStatusPhase::Initializing => Self::Provisioning("Initializing".into()),
            DpuStatusPhase::NodeEffect => Self::NodeEffect,
            DpuStatusPhase::Pending => Self::Provisioning("Pending".into()),
            DpuStatusPhase::ConfigFwParameters => Self::Provisioning("ConfigFwParameters".into()),
            DpuStatusPhase::PrepareBfb => Self::Provisioning("PrepareBfb".into()),
            DpuStatusPhase::OsInstalling => Self::Provisioning("OsInstalling".into()),
            DpuStatusPhase::DpuClusterConfig => Self::Provisioning("DpuClusterConfig".into()),
            DpuStatusPhase::HostNetworkConfiguration => {
                Self::Provisioning("HostNetworkConfiguration".into())
            }
            DpuStatusPhase::Ready => Self::Ready,
            DpuStatusPhase::Error => Self::Error,
            DpuStatusPhase::Deleting => Self::Deleting,
            DpuStatusPhase::Rebooting => Self::Rebooting,
            DpuStatusPhase::InitializeInterface => Self::Provisioning("InitializeInterface".into()),
            DpuStatusPhase::CheckingHostRebootRequired => Self::Rebooting,
            DpuStatusPhase::NodeEffectRemoval => Self::NodeEffect,
            DpuStatusPhase::DpuConfig => Self::Provisioning("DpuConfig".into()),
            DpuStatusPhase::PerformArmForceRestart => {
                Self::Provisioning("PerformArmForceRestart".into())
            }
            DpuStatusPhase::UpdateFirmware => Self::Provisioning("UpdateFirmware".into()),
            DpuStatusPhase::HostOsInitRelease => Self::Provisioning("HostOsInitRelease".into()),
        }
    }
}

/// Event emitted on any DPU resource change.
///
/// This event fires for every observed update to a DPU, not only when the
/// phase transitions. Handlers must be idempotent and tolerate receiving
/// the same phase multiple times.
#[derive(Debug, Clone)]
pub struct DpuEvent {
    /// Name of the DPU resource.
    pub dpu_name: String,
    /// DPU device name (DPUDevice CR name; matches operator label dpudevice-name).
    pub device_name: String,
    /// Name of the DPUNode containing this DPU.
    pub node_name: String,
    /// Observed phase.
    pub phase: DpuPhase,
}

/// Event emitted when a DPU is in the Rebooting phase.
#[derive(Debug, Clone)]
pub struct RebootRequiredEvent {
    /// Name of the DPU resource.
    pub dpu_name: String,
    /// Name of the DPUNode resource.
    pub node_name: String,
    /// Host BMC IP.
    pub host_bmc_ip: IpAddr,
}

/// Event emitted when a DPU is in the NodeEffect phase.
#[derive(Debug, Clone)]
pub struct MaintenanceEvent {
    /// Name of the DPU resource.
    pub dpu_name: String,
    /// Name of the DPUNode resource.
    pub node_name: String,
}

/// Event emitted when a DPU is in the Ready phase.
#[derive(Debug, Clone)]
pub struct DpuReadyEvent {
    /// Name of the DPU resource.
    pub dpu_name: String,
    /// DPU device name (DPUDevice CR name).
    pub device_name: String,
    /// Name of the DPUNode containing this DPU.
    pub node_name: String,
}

/// Event emitted when a DPU is in the Error phase.
#[derive(Debug, Clone)]
pub struct DpuErrorEvent {
    /// Name of the DPU resource.
    pub dpu_name: String,
    /// DPU device name (DPUDevice CR name).
    pub device_name: String,
    /// Name of the DPUNode containing this DPU.
    pub node_name: String,
}

/// Curated snapshot of the DPF CRs related to a single host. Produced by
/// [`crate::DpfSdk::snapshot_host`]. Designed for ad-hoc inspection (e.g.
/// printing as JSON from an admin CLI), not as a stable wire format.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostDpfSnapshot {
    pub dpu_node: Option<DpuNodeSummary>,
    pub dpu_devices: Vec<DpuDeviceSummary>,
    pub dpus: Vec<DpuSummary>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DpuNodeSummary {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub dpu_device_refs: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DpuDeviceSummary {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub bmc_ip: Option<String>,
    pub bmc_port: Option<i32>,
    pub serial_number: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DpuSummary {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub spec_bfb: String,
    pub spec_dpu_flavor: Option<String>,
    pub spec_dpu_device_name: String,
    pub spec_dpu_node_name: String,
    pub status_phase: Option<String>,
    pub status_bfb_file: Option<String>,
    /// `status.conditions`, verbatim. `phase` alone says where a DPU is, not
    /// why it is stuck there; the conditions carry the reason and message.
    pub status_conditions: Option<Vec<Condition>>,
    /// `status.operationalConditions`, verbatim. Separate from `conditions`:
    /// these describe the DPU's health once provisioned, rather than the
    /// progress of provisioning itself.
    pub status_operational_conditions: Option<Vec<DpuStatusOperationalConditions>>,
    /// `status.agentStatus`, verbatim. What the DPU-side agent reports about
    /// itself, including its own conditions, kubelet version, and reboot state.
    pub status_agent_status: Option<DpuStatusAgentStatus>,
}

/// Service version resolved from a DPUDeployment's services and their DPUServiceTemplate CRs.
/// Used by [`crate::DpfSdk::get_service_versions_for_dpu`] to populate the DPU inventory.
#[derive(Debug, Clone)]
pub struct DpuServiceVersion {
    /// Image case: basename after the final `/` of `helmChart.values.image.repository`.
    /// Helm fallback: `helmChart.source.chart`.
    pub name: String,
    /// Image case: `helmChart.values.image.tag`.
    /// Helm fallback: `helmChart.source.version`.
    pub version: String,
    /// Image case: `helmChart.values.image.repository` up to (not including) the final `/`.
    /// Helm fallback: `helmChart.source.repoURL`.
    pub url: String,
}

/// Helm-chart version observed on a live `DPUServiceTemplate` CR. Used by
/// [`crate::DpfSdk::list_service_template_versions`] so callers (e.g. the
/// admin CLI) can compare configured vs deployed versions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceTemplateVersion {
    pub cr_name: String,
    pub deployment_service_name: String,
    pub helm_repo_url: String,
    pub helm_chart: Option<String>,
    pub helm_version: String,
    /// Docker image tag extracted from `helm_chart.values.image.tag`, if
    /// present. Empty when the template doesn't pin an image (e.g. dts
    /// relies on the chart default).
    pub docker_image_tag: String,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// Provides one normalized intercept-bridging interface for topology validation cases.
    fn intercept_bridge(
        controller_id: u8,
        pf_id: u8,
        vf_id: Option<u8>,
        bridge: &str,
        patch_port: &str,
    ) -> DpfInterceptBridge {
        DpfInterceptBridge::new(
            DpfInterfaceIdentity {
                controller_id,
                pf_id,
                vf_id,
            },
            bridge,
            patch_port,
        )
    }

    /// Verifies all DPF intercept bridging identity, boundary, and rendered-name rejection rules.
    #[test]
    fn dpf_intercept_bridging_validates_identity_and_ovs_contracts() {
        value_scenarios!(
            run = |(interfaces, num_of_vfs)| DpfInterceptBridging::new(interfaces, num_of_vfs).is_ok();
            "empty replacement topology" {
                // FMDS cannot start when a configured replacement inventory omits its PF.
                (vec![], 0) => false,
            }

            "VF-only replacement topology" {
                // A VF cannot supply the single PF endpoint required by FMDS.
                (vec![intercept_bridge(2, 3, Some(4), "br-vf", "p-vf")], 16) => false,
            }

            "PF independent of VF count" {
                // PF selections remain valid even when hardware provisioning creates no VFs.
                (vec![intercept_bridge(2, 3, None, "br-pf", "p-pf")], 0) => true,
            }

            "VF below configured population" {
                // The highest provisioned VF index is accepted.
                (
                    vec![
                        intercept_bridge(2, 3, None, "br-pf", "p-pf"),
                        intercept_bridge(2, 3, Some(15), "br-vf", "p-vf"),
                    ],
                    16,
                ) => true,
            }

            "maximum typed VF endpoint" {
                // Controller/PF identity remains fully typed while VM addressing stops at VF15.
                (
                    vec![
                        intercept_bridge(255, 255, None, "br-pf", "p-pf"),
                        intercept_bridge(255, 255, Some(15), "br-vf", "p-vf"),
                    ],
                    126,
                ) => true,
            }

            "VF at configured population" {
                // VF indices are zero-based, so equality with num_of_vfs is out of range.
                (
                    vec![
                        intercept_bridge(2, 3, None, "br-pf", "p-pf"),
                        intercept_bridge(2, 3, Some(16), "br-vf", "p-vf"),
                    ],
                    16,
                ) => false,
            }

            "VF outside configured population below instance limit" {
                // VF8 is valid for VM addressing but absent from a four-VF hardware population.
                (
                    vec![
                        intercept_bridge(2, 3, None, "br-pf", "p-pf"),
                        intercept_bridge(2, 3, Some(8), "br-vf", "p-vf"),
                    ],
                    4,
                ) => false,
            }

            "VF above instance addressing limit" {
                // Additional hardware VFs are not valid VMaaS topology endpoints.
                (
                    vec![
                        intercept_bridge(2, 3, None, "br-pf", "p-pf"),
                        intercept_bridge(2, 3, Some(16), "br-vf", "p-vf"),
                    ],
                    126,
                ) => false,
            }

            "hardware VF population above platform limit" {
                // BlueField provisioning supports at most 126 hardware VFs per PF.
                (vec![intercept_bridge(2, 3, None, "br-pf", "p-pf")], 127) => false,
            }

            "duplicate typed identity" {
                // Different legacy entries cannot select the same DPF interface twice.
                (
                    vec![
                        intercept_bridge(2, 3, None, "br-pf", "p-pf"),
                        intercept_bridge(2, 3, Some(4), "br-a", "p-a"),
                        intercept_bridge(2, 3, Some(4), "br-b", "p-b"),
                    ],
                    16,
                ) => false,
            }

            "multiple selected parents" {
                // Service runtimes expose one logical PF0, so entries cannot span hardware parents.
                (
                    vec![
                        intercept_bridge(1, 3, None, "br-a", "p-a"),
                        intercept_bridge(2, 3, Some(4), "br-b", "p-b"),
                    ],
                    16,
                ) => false,
            }

            "long OVS patch port" {
                // Patch ports are OVSDB objects and do not inherit Linux's 15-byte netdev limit.
                (
                    vec![intercept_bridge(
                        1,
                        2,
                        None,
                        "br-host",
                        "patch-br-host-to-hbn",
                    )],
                    16,
                ) => true,
            }

            "DPF generated patch-port grammar" {
                // The checked-in CRD documents underscore-containing autogenerated patch names.
                (
                    vec![intercept_bridge(
                        1,
                        2,
                        None,
                        "br-host",
                        "p_brovn_to_brsfc_7aea60f7",
                    )],
                    16,
                ) => true,
            }

            "duplicate bridge" {
                // One intermediate bridge cannot own two independently managed Patch peers.
                (
                    vec![
                        intercept_bridge(1, 2, None, "br-pf", "p-pf"),
                        intercept_bridge(1, 2, Some(3), "br-shared", "p-a"),
                        intercept_bridge(1, 2, Some(4), "br-shared", "p-b"),
                    ],
                    16,
                ) => false,
            }

            "bridge and patch collision" {
                // Bridge and port names share OVS's global object namespace in this topology.
                (
                    vec![
                        intercept_bridge(1, 2, None, "br-pf", "p-pf"),
                        intercept_bridge(1, 2, Some(3), "br-a", "p-shared"),
                        intercept_bridge(1, 2, Some(4), "p-shared", "p-b"),
                    ],
                    16,
                ) => false,
            }

            "DPF-generated local patch collision" {
                // DPF derives the br-sfc-side port from the configured peer patch name.
                (
                    vec![
                        intercept_bridge(1, 2, None, "br-pf", "x"),
                        intercept_bridge(1, 2, Some(3), "br-vf", "p_brsfc_to_x"),
                    ],
                    16,
                ) => false,
            }

            "omitted provisioned VF representor collision" {
                // NUM_OF_VFS materializes VF4 even when the sparse inventory does not select it.
                (vec![intercept_bridge(1, 2, None, "pf2vf4", "p-pf")], 16) => false,
            }

            "reserved DPF bridge" {
                // An intermediate bridge must not alias DPF's service-chain bridge.
                (vec![intercept_bridge(1, 2, None, "br-sfc", "p-a")], 16) => false,
            }

            "unsafe shell character" {
                // Restrictive validation prevents shell interpolation and OVS ambiguity.
                (vec![intercept_bridge(1, 2, None, "br_bad", "p-a")], 16) => false,
            }

            "uppercase name" {
                // The installed cross-surface contract uses lowercase names only.
                (vec![intercept_bridge(1, 2, None, "Br-bad", "p-a")], 16) => false,
            }

            "unsafe patch-port character" {
                // Patch names are rendered into shell-safe flavor data and cannot contain quotes.
                (vec![intercept_bridge(1, 2, None, "br-host", "p'bad")], 16) => false,
            }

            "name exceeds Linux netdev limit" {
                // Linux-backed OVS ports and bridges must fit the 15-character interface name.
                (vec![intercept_bridge(1, 2, None, "abcdefghijklmnop", "p-a")], 16) => false,
            }
        );

        // A valid-range VM VF must reach the hardware-population gate rather than the VF15 gate.
        assert!(matches!(
            DpfInterceptBridging::new(
                vec![
                    intercept_bridge(2, 3, None, "br-pf", "p-pf"),
                    intercept_bridge(2, 3, Some(8), "br-vf", "p-vf"),
                ],
                4,
            ),
            Err(crate::DpfError::ConfigError(message))
                if message
                    == "DPF intercept-bridging interface c2pf3vf8 selects a VF outside num_of_vfs=4"
        ));
    }

    /// Verifies normalization order and generated names depend only on typed identity.
    #[test]
    fn dpf_intercept_bridging_sorts_by_typed_identity() {
        // Supply entries in reverse identity order.
        let topology = DpfInterceptBridging::new(
            vec![
                intercept_bridge(2, 1, Some(3), "br-b", "p-b"),
                intercept_bridge(2, 1, None, "br-a", "p-a"),
            ],
            16,
        )
        .expect("valid topology must normalize");

        // Stable names and sorting prevent needless immutable flavor hash churn.
        assert_eq!(
            topology
                .interfaces()
                .iter()
                .map(|interface| interface.identity.resource_name())
                .collect::<Vec<_>>(),
            ["c2pf1", "c2pf1vf3"]
        );
    }

    /// `DpuPhase::from(DpuStatusPhase)` is a total conversion; every operator
    /// status phase maps to exactly one simplified `DpuPhase`. This folds the
    /// old `test_dpu_phase_from_status` and enumerates all 17 source variants,
    /// including each provisioning sub-phase that collapses into
    /// `Provisioning(detail)`.
    #[test]
    fn dpu_phase_from_status_maps_every_variant() {
        value_scenarios!(
            run = DpuPhase::from;
            "Initializing -> Provisioning" {
                DpuStatusPhase::Initializing => DpuPhase::Provisioning("Initializing".into()),
            }

            "Pending -> Provisioning" {
                DpuStatusPhase::Pending => DpuPhase::Provisioning("Pending".into()),
            }

            "ConfigFwParameters -> Provisioning" {
                DpuStatusPhase::ConfigFwParameters => DpuPhase::Provisioning("ConfigFwParameters".into()),
            }

            "PrepareBfb -> Provisioning" {
                DpuStatusPhase::PrepareBfb => DpuPhase::Provisioning("PrepareBfb".into()),
            }

            "OsInstalling -> Provisioning" {
                DpuStatusPhase::OsInstalling => DpuPhase::Provisioning("OsInstalling".into()),
            }

            "DpuClusterConfig -> Provisioning" {
                DpuStatusPhase::DpuClusterConfig => DpuPhase::Provisioning("DpuClusterConfig".into()),
            }

            "HostNetworkConfiguration -> Provisioning" {
                DpuStatusPhase::HostNetworkConfiguration => DpuPhase::Provisioning("HostNetworkConfiguration".into()),
            }

            "InitializeInterface -> Provisioning" {
                DpuStatusPhase::InitializeInterface => DpuPhase::Provisioning("InitializeInterface".into()),
            }

            "DpuConfig -> Provisioning" {
                DpuStatusPhase::DpuConfig => DpuPhase::Provisioning("DpuConfig".into()),
            }

            "PerformArmForceRestart -> Provisioning" {
                DpuStatusPhase::PerformArmForceRestart => DpuPhase::Provisioning("PerformArmForceRestart".into()),
            }

            "NodeEffect -> NodeEffect" {
                DpuStatusPhase::NodeEffect => DpuPhase::NodeEffect,
            }

            "NodeEffectRemoval -> NodeEffect" {
                DpuStatusPhase::NodeEffectRemoval => DpuPhase::NodeEffect,
            }

            "Rebooting -> Rebooting" {
                DpuStatusPhase::Rebooting => DpuPhase::Rebooting,
            }

            "CheckingHostRebootRequired -> Rebooting" {
                DpuStatusPhase::CheckingHostRebootRequired => DpuPhase::Rebooting,
            }

            "Ready -> Ready" {
                DpuStatusPhase::Ready => DpuPhase::Ready,
            }

            "Error -> Error" {
                DpuStatusPhase::Error => DpuPhase::Error,
            }

            "Deleting -> Deleting" {
                DpuStatusPhase::Deleting => DpuPhase::Deleting,
            }
        );
    }

    /// `AsRef<str>` for `DpuPhase` renders each variant to its canonical name;
    /// a provisioning phase renders its detail string verbatim. Covers all six
    /// `DpuPhase` variants, including an empty-detail provisioning phase.
    ///
    /// `Display` delegates to `AsRef<str>`, so each row also asserts that
    /// `to_string()` agrees with `as_ref()` before yielding the rendered name —
    /// folding in the former `dpu_phase_display_matches_as_ref`.
    #[test]
    fn dpu_phase_as_ref_renders_each_variant() {
        value_scenarios!(
            run = |phase: DpuPhase| {
                let as_ref = phase.as_ref().to_string();
                assert_eq!(phase.to_string(), as_ref, "Display must match AsRef");
                as_ref
            };
            "provisioning renders its detail" {
                DpuPhase::Provisioning("OsInstalling".into()) => "OsInstalling".to_string(),
            }

            "provisioning with empty detail renders empty" {
                DpuPhase::Provisioning(String::new()) => String::new(),
            }

            "node effect" {
                DpuPhase::NodeEffect => "NodeEffect".to_string(),
            }

            "rebooting" {
                DpuPhase::Rebooting => "Rebooting".to_string(),
            }

            "ready" {
                DpuPhase::Ready => "Ready".to_string(),
            }

            "error" {
                DpuPhase::Error => "Error".to_string(),
            }

            "deleting" {
                DpuPhase::Deleting => "Deleting".to_string(),
            }
        );
    }

    /// `InitDpfResourcesConfig::default()` seeds the documented defaults: an
    /// empty BFB URL and inventories, the `dpu-deployment` name, the crate
    /// default flavor, 16 VFs, 30 reserved SFs, no intercept-bridging topology,
    /// and no proxy. Probe each field independently.
    #[test]
    fn init_dpf_resources_config_default_fields() {
        value_scenarios!(
            run = |()| InitDpfResourcesConfig::default().bfb_url.is_empty();
            "bfb url is empty" {
                () => true,
            }
        );
        value_scenarios!(
            run = |()| InitDpfResourcesConfig::default().deployment_name;
            "deployment name" {
                () => "dpu-deployment".to_string(),
            }
        );
        value_scenarios!(
            run = |()| InitDpfResourcesConfig::default().flavor_name;
            "flavor name uses crate default" {
                () => crate::flavor::DEFAULT_FLAVOR_NAME.to_string(),
            }
        );
        value_scenarios!(
            run = |()| InitDpfResourcesConfig::default().services.len();
            "services is empty" {
                () => 0usize,
            }
        );
        value_scenarios!(
            run = |()| {
                let config = InitDpfResourcesConfig::default();
                (
                    config.num_of_vfs,
                    config.pf_total_sf_reserved,
                    config.deployment_scoped_service_interfaces,
                    config.intercept_bridging.is_none(),
                    config.interfaces.is_empty(),
                )
            };
            "interface projection defaults" {
                // Existing SDK callers retain the historical static 16-VF behavior.
                () => (
                    DEFAULT_DPU_NUM_OF_VFS,
                    DEFAULT_PF_TOTAL_SF_RESERVED,
                    false,
                    true,
                    true,
                ),
            }
        );
        value_scenarios!(
            run = |()| InitDpfResourcesConfig::default().proxy.is_none();
            "proxy is none" {
                () => true,
            }
        );
    }
}
