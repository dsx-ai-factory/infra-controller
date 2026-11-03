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

//! Carbide-specific DPU service definitions for DPUServiceTemplate / DPUServiceConfiguration.

use std::collections::BTreeMap;
use std::fmt::Write;

use carbide_dpf::types::{
    DHCP_SERVER_SERVICE_NAME, DOCA_HBN_SERVICE_NAME, DOCA_HBN_SERVICE_NETWORK,
    DOCA_WEAVE_DHCP_AGENT_SERVICE_NAME, DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_NAME,
    DOCA_XPLANE_SERVICE_NAME, DPU_AGENT_SERVICE_NAME, DTS_SERVICE_NAME,
    DpuServiceInterfaceTemplateDefinition, FMDS_SERVICE_NAME, OTEL_COLLECTOR_SERVICE_NAME,
};
use carbide_dpf::{
    DetachedDpuServiceDefinition, DetachedDpuServiceSecurity, DetachedHelmChart, DpfError,
    DpuServiceObservation, IntOrString, ServiceDefinition, ServiceInterface, ServiceNAD,
    ServiceNADResourceType, ServiceVpcSlots,
};
#[cfg(test)]
use carbide_dpf::{
    DpuServiceDaemonSetObservation, DpuServiceHelmChartObservation, DpuServiceSecurityObservation,
};
use carbide_extension_service_controller::dpu_service::{
    dpu_service_mutable_fields_match, dpu_service_mutable_patch,
};
use carbide_machine_controller::dpf::DpfOperations;

use crate::cfg::file::{
    DpfBootstrapCaObjectKind, DpfDpuAgentBootstrapCa, DpfExtraService,
    DpfResolvedMandatoryServicesConfig, DpfServiceConfig, EwEthersConfig, NodeAuthConfig,
};

/// Default DOCA helm registry (DPUServiceTemplate source.repoURL).
pub(crate) const DEFAULT_DOCA_HELM_REGISTRY: &str = "https://helm.ngc.nvidia.com/nvidia/doca";

pub(crate) const DEFAULT_CARBIDE_HELM_REGISTRY: &str =
    "https://helm.ngc.nvidia.com/0837451325059433/carbide-dev";

/// Default DOCA container image registry prefix.
pub(crate) const DEFAULT_DOCA_IMAGE_REGISTRY: &str = "nvcr.io/nvidia/doca";

/// Default Carbide container image registry prefix.
pub(crate) const DEFAULT_CARBIDE_IMAGE_REGISTRY: &str = "nvcr.io/0837451325059433/carbide-dev";

/// Astra Service Helm and Image Registries
pub(crate) const DOCA_WEAVE_CHART_REPO_URL: &str = "oci://nvcr.io/nvstaging/doca";
pub(crate) const DOCA_WEAVE_IMAGE_REGISTRY: &str = "nvcr.io/nvstaging/doca";

const DOCA_XPLANE_CHART_REPO_URL: &str = "https://helm.ngc.nvidia.com/nvstaging/doca";
const DOCA_XPLANE_IMAGE_REGISTRY: &str = "nvcr.io/nvstaging/doca";

/// HBN service Definitions
pub(crate) const DOCA_HBN_SERVICE_HELM_NAME: &str = "doca-hbn";
pub(crate) const DOCA_HBN_SERVICE_HELM_VERSION: &str = "3.4.0";
pub(crate) const DOCA_HBN_SERVICE_IMAGE_NAME: &str = "doca_hbn";
pub(crate) const DOCA_HBN_SERVICE_IMAGE_TAG: &str = "3.4.0-doca3.4.0";

/// DHCP Service Definitions
pub(crate) const DHCP_SERVER_SERVICE_HELM_NAME: &str = "nico-dhcp-server";
pub(crate) const DHCP_SERVER_SERVICE_NAD_NAME: &str = "mybrsfc-dhcp";
pub(crate) const DHCP_SERVER_SERVICE_MTU: i64 = 1500;
pub(crate) const DHCP_SERVER_SERVICE_IMAGE_NAME: &str = "forge-dhcp-server";

/// Stable DPUService and Helm release that owns shared service-VPC network resources.
pub(crate) const PLATFORM_SERVICE_VPC_NAME: &str = "nico-service-vpc";
/// Fixed IPv4 NAD name referenced by extension-service consumers.
#[cfg(test)]
const SERVICE_VPC_IPV4_NAD_NAME: &str = "nico-service-vpc-ipv4";
/// Label that distinguishes Core's stable platform DPUServices from tenant extension services.
pub(crate) const PLATFORM_SERVICE_OWNER_LABEL: &str = "carbide.nvidia.com/platform-service";
/// Ownership value reserved for the shared service-VPC network release.
pub(crate) const PLATFORM_SERVICE_OWNER_VALUE: &str = "service-vpc";

/// DTS service definitions
/// (DTS_SERVICE_NAME lives in carbide_dpf::types so the DPF SDK can wire its dependencies.)
pub(crate) const DTS_SERVICE_HELM_NAME: &str = "doca-telemetry";
pub(crate) const DTS_SERVICE_HELM_VERSION: &str = "1.25.5";

// DPU Agent Service Definitions
pub(crate) const DPU_AGENT_SERVICE_HELM_NAME: &str = "nico-dpu-agent";
pub(crate) const DPU_AGENT_SERVICE_IMAGE_NAME: &str = "forge-dpu-agent";

/// FMDS Agent Service Definitions
pub(crate) const FMDS_SERVICE_HELM_NAME: &str = "nico-fmds";
pub(crate) const FMDS_SERVICE_IMAGE_NAME: &str = "carbide-fmds";
pub(crate) const FMDS_SERVICE_NAD_NAME: &str = "mybrsfc-fmds";
pub(crate) const FMDS_SERVICE_MTU: i64 = 1500;

/// OTel Collector Service Definitions
pub(crate) const OTEL_COLLECTOR_SERVICE_HELM_NAME: &str = "nico-otelcol";
pub(crate) const OTEL_COLLECTOR_SERVICE_IMAGE_NAME: &str = "otelcol-contrib";

/// Built-in defaults for the Weave DHCP agent extra service.
///
/// Sites can override these fields under `[dpf.extra_services.doca_weave_dhcp_agent]`;
/// deployment-local overrides under
/// `[dpf.deployments.<name>.extra_services.doca_weave_dhcp_agent]` take precedence. Keep
/// defaults here so enabling Astra does not require every site to restate the chart and image.
pub(crate) const DOCA_WEAVE_DHCP_AGENT_SERVICE_HELM_NAME: &str = "dpf-weave";
pub(crate) const DOCA_WEAVE_DHCP_AGENT_SERVICE_HELM_VERSION: &str = "v26.8.0-d464e7bf-nightly";
pub(crate) const DOCA_WEAVE_DHCP_AGENT_SERVICE_IMAGE_NAME: &str = "weave-system";
pub(crate) const DOCA_WEAVE_DHCP_AGENT_SERVICE_IMAGE_TAG: &str = "v26.8.0-d464e7bf-nightly";

/// Built-in defaults for the Weave flow-controller extra service.
///
/// Sites can override these fields under `[dpf.extra_services.doca_weave_flow_controller]`;
/// deployment-local overrides under
/// `[dpf.deployments.<name>.extra_services.doca_weave_flow_controller]` take precedence.
pub(crate) const DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_HELM_NAME: &str = "dpf-weave";
pub(crate) const DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_HELM_VERSION: &str = "v26.8.0-d464e7bf-nightly";
pub(crate) const DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_IMAGE_NAME: &str = "weave-system";
pub(crate) const DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_IMAGE_TAG: &str = "v26.8.0-d464e7bf-nightly";

/// Built-in defaults for the Xplane extra service.
///
/// Sites can override these fields under `[dpf.extra_services.doca_xplane]`;
/// deployment-local overrides under `[dpf.deployments.<name>.extra_services.doca_xplane]`
/// take precedence. Keep defaults here so enabling Astra does not require every site to restate
/// the chart and image.
const DOCA_XPLANE_SERVICE_HELM_NAME: &str = "xplane";
const DOCA_XPLANE_SERVICE_HELM_VERSION: &str = "3.5.0020";
const DOCA_XPLANE_SERVICE_IMAGE_NAME: &str = "xplane";
const DOCA_XPLANE_SERVICE_IMAGE_TAG: &str = "3.5.0020";

/// Compile-time Helm version supplied explicitly or derived from Git; empty only when neither
/// source is available.
pub(crate) const COMPILE_TIME_HELM_VERSION: &str = match option_env!("CARBIDE_BUILD_HELM_VERSION") {
    Some(v) => v,
    None => "",
};

/// Compile-time image tag (set by CI via VERSION env var). Empty on PR/fork builds.
pub(crate) const COMPILE_TIME_IMAGE_TAG: &str = match option_env!("CARBIDE_BUILD_GIT_TAG") {
    Some(v) => v,
    None => "",
};

fn doca_hbn_service_interfaces(
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
    service_vpc_slots: ServiceVpcSlots,
) -> Vec<ServiceInterface> {
    let mut service_interfaces =
        dpu_service_interfaces(interfaces, DOCA_HBN_SERVICE_NAME, DOCA_HBN_SERVICE_NETWORK);
    service_vpc_slots.append_hbn_interfaces(&mut service_interfaces);
    service_interfaces
}

fn dhcp_server_service_interfaces(
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
) -> Vec<ServiceInterface> {
    dpu_service_interfaces(
        interfaces,
        DHCP_SERVER_SERVICE_NAME,
        DHCP_SERVER_SERVICE_NAD_NAME,
    )
}
fn fmds_service_interfaces(
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
) -> Vec<ServiceInterface> {
    dpu_service_interfaces(interfaces, FMDS_SERVICE_NAME, FMDS_SERVICE_NAD_NAME)
}

fn dpu_service_interfaces(
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
    service_name: &str,
    network: &str,
) -> Vec<ServiceInterface> {
    // Service definitions consume only endpoints declared by the shared effective inventory.
    interfaces
        .iter()
        .filter_map(|iface| {
            iface.chained_svc_if.as_ref().and_then(|chains| {
                chains
                    .iter()
                    .find_map(|(chained_service_name, interface_name)| {
                        (chained_service_name == service_name).then(|| ServiceInterface {
                            name: interface_name.clone(),
                            network: network.to_string(),
                        })
                    })
            })
        })
        .collect()
}

fn doca_hbn_startup_yaml(interfaces: &[ServiceInterface]) -> String {
    let mut startup_yaml = String::from(concat!(
        "- header:\n",
        "    model: BLUEFIELD\n",
        "    nvue-api-version: nvue_v1\n",
        "    rev-id: 1.0\n",
        "    version: HBN 2.4.0\n",
        "- set:\n",
        "    system:\n",
        "      api:\n",
        "        listening-address:\n",
        "          0.0.0.0: {}\n",
        "    interface:\n",
    ));

    for interface in interfaces {
        let _ = writeln!(startup_yaml, "      {}:", interface.name);
        startup_yaml.push_str("        type: swp\n");
    }

    startup_yaml
}

pub(crate) fn default_dts_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: DTS_SERVICE_NAME.to_string(),
        helm_repo_url: DEFAULT_DOCA_HELM_REGISTRY.to_string(),
        helm_chart: DTS_SERVICE_HELM_NAME.to_string(),
        helm_version: DTS_SERVICE_HELM_VERSION.to_string(),
        docker_repo_url: String::new(),
        docker_image_tag: String::new(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_doca_hbn_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: DOCA_HBN_SERVICE_NAME.to_string(),
        helm_repo_url: DEFAULT_DOCA_HELM_REGISTRY.to_string(),
        helm_chart: DOCA_HBN_SERVICE_HELM_NAME.to_string(),
        helm_version: DOCA_HBN_SERVICE_HELM_VERSION.to_string(),
        docker_repo_url: format!("{DEFAULT_DOCA_IMAGE_REGISTRY}/{DOCA_HBN_SERVICE_IMAGE_NAME}"),
        docker_image_tag: DOCA_HBN_SERVICE_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_dpu_agent_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: DPU_AGENT_SERVICE_NAME.to_string(),
        helm_repo_url: DEFAULT_CARBIDE_HELM_REGISTRY.to_string(),
        helm_chart: DPU_AGENT_SERVICE_HELM_NAME.to_string(),
        helm_version: COMPILE_TIME_HELM_VERSION.to_string(),
        docker_repo_url: format!("{DEFAULT_CARBIDE_IMAGE_REGISTRY}/{DPU_AGENT_SERVICE_IMAGE_NAME}"),
        docker_image_tag: COMPILE_TIME_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_dhcp_server_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: DHCP_SERVER_SERVICE_NAME.to_string(),
        helm_repo_url: DEFAULT_CARBIDE_HELM_REGISTRY.to_string(),
        helm_chart: DHCP_SERVER_SERVICE_HELM_NAME.to_string(),
        helm_version: COMPILE_TIME_HELM_VERSION.to_string(),
        docker_repo_url: format!(
            "{DEFAULT_CARBIDE_IMAGE_REGISTRY}/{DHCP_SERVER_SERVICE_IMAGE_NAME}"
        ),
        docker_image_tag: COMPILE_TIME_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_fmds_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: FMDS_SERVICE_NAME.to_string(),
        helm_repo_url: DEFAULT_CARBIDE_HELM_REGISTRY.to_string(),
        helm_chart: FMDS_SERVICE_HELM_NAME.to_string(),
        helm_version: COMPILE_TIME_HELM_VERSION.to_string(),
        docker_repo_url: format!("{DEFAULT_CARBIDE_IMAGE_REGISTRY}/{FMDS_SERVICE_IMAGE_NAME}"),
        docker_image_tag: COMPILE_TIME_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_otelcol_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: OTEL_COLLECTOR_SERVICE_NAME.to_string(),
        helm_repo_url: DEFAULT_CARBIDE_HELM_REGISTRY.to_string(),
        helm_chart: OTEL_COLLECTOR_SERVICE_HELM_NAME.to_string(),
        helm_version: COMPILE_TIME_HELM_VERSION.to_string(),
        docker_repo_url: format!(
            "{DEFAULT_CARBIDE_IMAGE_REGISTRY}/{OTEL_COLLECTOR_SERVICE_IMAGE_NAME}"
        ),
        docker_image_tag: COMPILE_TIME_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_doca_weave_dhcp_agent_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: DOCA_WEAVE_DHCP_AGENT_SERVICE_NAME.to_string(),
        helm_repo_url: DOCA_WEAVE_CHART_REPO_URL.to_string(),
        helm_chart: DOCA_WEAVE_DHCP_AGENT_SERVICE_HELM_NAME.to_string(),
        helm_version: DOCA_WEAVE_DHCP_AGENT_SERVICE_HELM_VERSION.to_string(),
        docker_repo_url: format!(
            "{DOCA_WEAVE_IMAGE_REGISTRY}/{DOCA_WEAVE_DHCP_AGENT_SERVICE_IMAGE_NAME}"
        ),
        docker_image_tag: DOCA_WEAVE_DHCP_AGENT_SERVICE_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_doca_weave_flow_controller_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_NAME.to_string(),
        helm_repo_url: DOCA_WEAVE_CHART_REPO_URL.to_string(),
        helm_chart: DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_HELM_NAME.to_string(),
        helm_version: DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_HELM_VERSION.to_string(),
        docker_repo_url: format!(
            "{DOCA_WEAVE_IMAGE_REGISTRY}/{DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_IMAGE_NAME}"
        ),
        docker_image_tag: DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

pub(crate) fn default_doca_xplane_service() -> DpfServiceConfig {
    DpfServiceConfig {
        name: DOCA_XPLANE_SERVICE_NAME.to_string(),
        helm_repo_url: DOCA_XPLANE_CHART_REPO_URL.to_string(),
        helm_chart: DOCA_XPLANE_SERVICE_HELM_NAME.to_string(),
        helm_version: DOCA_XPLANE_SERVICE_HELM_VERSION.to_string(),
        docker_repo_url: format!("{DOCA_XPLANE_IMAGE_REGISTRY}/{DOCA_XPLANE_SERVICE_IMAGE_NAME}"),
        docker_image_tag: DOCA_XPLANE_SERVICE_IMAGE_TAG.to_string(),
        docker_image_pull_secret: None,
        extra_helm_values: None,
    }
}

/// Adds an `imagePullSecrets` block to a service's Helm values when the service has
/// a configured pull secret, omitting it entirely otherwise. Mandatory services carry
/// no pull secret by default (public-registry pulls); one is emitted only when set via
/// the service's per-service config or the top-level `docker_image_pull_secret`
/// override, and a `None` renders no `imagePullSecrets` rather than an invalid null.
fn apply_image_pull_secrets(helm_values: &mut serde_json::Value, cfg: &DpfServiceConfig) {
    let Some(secret) = cfg.docker_image_pull_secret.as_ref() else {
        return;
    };
    helm_values
        .as_object_mut()
        .expect("service Helm values must be a JSON object")
        .insert(
            "imagePullSecrets".to_string(),
            serde_json::json!([{ "name": secret }]),
        );
}

fn merge_helm_values(
    base: &mut serde_json::Map<String, serde_json::Value>,
    overlay: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in overlay {
        match (base.get_mut(key), value) {
            (Some(serde_json::Value::Object(base)), serde_json::Value::Object(overlay)) => {
                merge_helm_values(base, overlay);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

fn apply_helm_values(helm_values: &mut serde_json::Value, cfg: &DpfServiceConfig) {
    apply_image_pull_secrets(helm_values, cfg);
    if let Some(overlay) = &cfg.extra_helm_values {
        merge_helm_values(
            helm_values
                .as_object_mut()
                .expect("generated Helm values must be an object"),
            overlay,
        );
    }
}

/// Restores the topology-derived HBN SF request after applying operator Helm overrides.
fn set_hbn_sf_count(helm_values: &mut serde_json::Value, sf_count: usize) {
    // `doca_hbn_service` constructs the root object locally. Operator overlays can replace its
    // fields, including `resources`, but cannot replace the generated root value.
    let helm_values = helm_values
        .as_object_mut()
        .expect("generated HBN Helm values must be an object");
    let resources = helm_values
        .entry("resources")
        .or_insert_with(|| serde_json::json!({}));
    match resources {
        serde_json::Value::Object(resources) => {
            resources.insert("nvidia.com/bf_sf".to_string(), serde_json::json!(sf_count));
        }
        resources => {
            *resources = serde_json::json!({
                "nvidia.com/bf_sf": sf_count,
            });
        }
    }
}

/// Restores a value the API owns after the operator overlay has been merged.
///
/// `apply_helm_values` merges `extra_helm_values` last, so an overlay wins over
/// anything generated above it. That is right for tuning a chart, and wrong for
/// the handful of values that encode an agreement between two components: the
/// API validates them at startup, so letting a Helm overlay change one deploys
/// a fleet the API will reject while passing every check. Reassert them, and
/// say so when an overlay tried.
fn reassert_api_owned_value(
    helm_values: &mut serde_json::Value,
    service: &str,
    path: &[&str],
    authoritative: serde_json::Value,
) {
    let (last, parents) = path.split_last().expect("path must not be empty");
    let mut node = helm_values;
    for key in parents {
        // An overlay can put anything here, including a scalar or null, so
        // descend defensively: assuming an object would turn a malformed
        // `extra_helm_values` into a panic during DPF resource creation, which
        // is a poor way to report a typo in someone's config.
        let entry = node
            .as_object_mut()
            .expect("generated Helm values must be an object")
            .entry((*key).to_string())
            .or_insert_with(|| serde_json::json!({}));
        if !entry.is_object() {
            *entry = serde_json::json!({});
        }
        node = entry;
    }
    let object = node
        .as_object_mut()
        .expect("generated Helm values must be an object");
    match object.get(*last) {
        Some(existing) if *existing == authoritative => {}
        Some(overridden) => tracing::warn!(
            target: "node_auth",
            service,
            setting = path.join("."),
            %overridden,
            authoritative = %authoritative,
            "node-auth: ignoring an extra_helm_values override of a value the API owns; \
             it is validated at startup and both ends must agree"
        ),
        None => {}
    }
    object.insert((*last).to_string(), authoritative);
}

/// DOCA HBN service definition.
pub(crate) fn doca_hbn_service(
    cfg: &DpfServiceConfig,
    dpu_interfaces: &[DpuServiceInterfaceTemplateDefinition],
    service_vpc_slots: ServiceVpcSlots,
) -> ServiceDefinition {
    let interfaces = doca_hbn_service_interfaces(dpu_interfaces, service_vpc_slots);
    let mut helm_values = serde_json::json!({
        "image": {
            "repository": cfg.docker_repo_url,
            "tag": cfg.docker_image_tag,
        },
        "resources": {
            "memory": "6Gi",
            "nvidia.com/bf_sf": interfaces.len(),
        },
        "configuration": {
            "user": {
                "create": true,
                "username": "carbide",
                "password": {
                    "secretName": "hbn-user-password",
                    "secretKey": "password",
                },
            },
        }
    });
    apply_helm_values(&mut helm_values, cfg);
    // Interface assignments, startup YAML, and the requested SF population are one contract.
    // An operator Helm overlay may customize other chart values but must not split that contract.
    set_hbn_sf_count(&mut helm_values, interfaces.len());
    ServiceDefinition {
        helm_values: Some(helm_values),

        config_values: Some(serde_json::json!({
            "configuration": {
                "startupYAMLJ2": doca_hbn_startup_yaml(&interfaces)
            }
        })),

        service_daemon_set_annotations: Some(BTreeMap::new()),

        interfaces,

        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

/// DTS (DOCA Telemetry Service) service definition.
pub(crate) fn dts_service(cfg: &DpfServiceConfig) -> ServiceDefinition {
    let mut helm_values = serde_json::json!({
        "exposedPorts": { "ports": { "httpserverport": true } }
    });
    // The public 1.25.5 chart supplies its image through `imageDTS`, whereas
    // newer charts accept the `image` map. Leave image selection to the chart
    // unless the site explicitly overrides its repository or tag.
    if !cfg.docker_repo_url.is_empty() || !cfg.docker_image_tag.is_empty() {
        helm_values["image"] = serde_json::json!({});
        if !cfg.docker_repo_url.is_empty() {
            helm_values["image"]["repository"] =
                serde_json::Value::String(cfg.docker_repo_url.clone());
        }
        if !cfg.docker_image_tag.is_empty() {
            helm_values["image"]["tag"] = serde_json::Value::String(cfg.docker_image_tag.clone());
        }
    }
    apply_helm_values(&mut helm_values, cfg);
    ServiceDefinition {
        helm_values: Some(helm_values),
        config_ports: None,
        config_ports_service_type: None,
        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

fn dpu_agent_helm_values(
    cfg: &DpfServiceConfig,
    bootstrap_ca: &DpfDpuAgentBootstrapCa,
) -> serde_json::Value {
    let mut values = serde_json::json!({
        "image": {
            "repository": cfg.docker_repo_url,
            "tag": cfg.docker_image_tag,
        },
        "hbn": {
            "nvue_https_address": "nvue",
            "nvue_credentials_secret_name": "hbn-user-password",
            "nvue_password_key": "password",
        }
    });
    let bootstrap_ca_values = match bootstrap_ca {
        DpfDpuAgentBootstrapCa::LegacyDownload { url: None } => None,
        DpfDpuAgentBootstrapCa::LegacyDownload { url: Some(url) } => Some(serde_json::json!({
            "source": "legacy_download",
            "url": url,
        })),
        DpfDpuAgentBootstrapCa::Mounted {
            object_kind,
            name,
            key,
        } => Some(serde_json::json!({
            "source": "mounted",
            "object": {
                "kind": match object_kind {
                    DpfBootstrapCaObjectKind::Secret => "Secret",
                    DpfBootstrapCaObjectKind::ConfigMap => "ConfigMap",
                },
                "name": name,
                "key": key,
            },
        })),
    };

    if let Some(bootstrap_ca_values) = bootstrap_ca_values {
        values
            .as_object_mut()
            .expect("DPU agent Helm values are an object")
            .insert("bootstrapCa".to_string(), bootstrap_ca_values);
    }
    apply_helm_values(&mut values, cfg);

    values
}

/// Forge DPU Agent service definition.
pub(crate) fn dpu_agent_service(
    cfg: &DpfServiceConfig,
    bootstrap_ca: &DpfDpuAgentBootstrapCa,
) -> ServiceDefinition {
    ServiceDefinition {
        helm_values: Some(dpu_agent_helm_values(cfg, bootstrap_ca)),
        apply_node_effect: Some(false),

        service_daemon_set_annotations: Some(BTreeMap::new()),

        config_values: Some(serde_json::json!({
            "dhcp_server": {
                "service_name": "{{ (index .Services \"carbide-dhcp-server\").Name }}",
                "interface_prepend": "d_"
            },
            "fmds": {
                "service_name": "{{ (index .Services \"carbide-fmds\").Name }}"
            },
            "hbn": {
                "nvue_https_address": "{{ (index .Services \"doca-hbn\").Name }}"
            }
        })),

        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

/// Builds shared DHCP chart values so deployment and platform releases use identical inputs.
fn dhcp_server_helm_values(cfg: &DpfServiceConfig) -> serde_json::Value {
    let mut helm_values = serde_json::json!({
        "image": {
            "repository": cfg.docker_repo_url,
            "tag": cfg.docker_image_tag,
        }
    });
    apply_helm_values(&mut helm_values, cfg);
    helm_values
}

/// Builds the DHCP service definition for a DPUDeployment, leaving legacy omission unchanged while
/// preventing an explicitly configured deployment release from rendering the fixed IPv4 NAD.
pub(crate) fn dhcp_server_service(
    cfg: &DpfServiceConfig,
    dpu_interfaces: &[DpuServiceInterfaceTemplateDefinition],
) -> ServiceDefinition {
    let mut helm_values = dhcp_server_helm_values(cfg);
    // Keep `serviceVpcNetworks.ipv4.enabled` absent when the operator omitted it, but replace any
    // explicit value with `false` so DPUDeployment releases cannot render `nico-service-vpc-ipv4`.
    if let Some(enabled) = helm_values
        .get_mut("serviceVpcNetworks")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|networks| networks.get_mut("ipv4"))
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|ipv4| ipv4.get_mut("enabled"))
    {
        *enabled = serde_json::json!(false);
    }
    ServiceDefinition {
        helm_values: Some(helm_values),

        interfaces: dhcp_server_service_interfaces(dpu_interfaces),
        apply_node_effect: Some(false),

        service_daemon_set_annotations: Some(BTreeMap::new()),

        service_nads: vec![ServiceNAD {
            name: DHCP_SERVER_SERVICE_NAD_NAME.to_string(),
            bridge: Some("br-sfc".to_string()),
            resource_type: ServiceNADResourceType::Sf,
            ipam: Some(false),
            mtu: Some(DHCP_SERVER_SERVICE_MTU),
        }],

        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

/// Builds the stable IPv4 service-VPC DPUService only for explicit management, preserving
/// deployment-only configurations while pinning the network runtime to Core's packaged chart.
pub(crate) fn platform_service_vpc_ipv4(
    cfg: &DpfServiceConfig,
    platform_helm_version: &str,
) -> Result<Option<DetachedDpuServiceDefinition>, DpfError> {
    // Omission preserves configurations that predate the stable platform release.
    let Some(service_vpc_networks) = cfg
        .extra_helm_values
        .as_ref()
        .and_then(|values| values.get("serviceVpcNetworks"))
    else {
        return Ok(None);
    };
    let service_vpc_networks = service_vpc_networks.as_object().ok_or_else(|| {
        DpfError::ConfigError(
            "dpf.services.dhcp_server.extra_helm_values.serviceVpcNetworks must be an object"
                .to_string(),
        )
    })?;
    let Some(ipv4) = service_vpc_networks.get("ipv4") else {
        return Ok(None);
    };
    let ipv4 = ipv4.as_object().ok_or_else(|| {
        DpfError::ConfigError(
            "dpf.services.dhcp_server.extra_helm_values.serviceVpcNetworks.ipv4 must be an object"
                .to_string(),
        )
    })?;
    let Some(enabled) = ipv4.get("enabled") else {
        return Ok(None);
    };
    if !enabled.is_boolean() {
        return Err(DpfError::ConfigError(
            "dpf.services.dhcp_server.extra_helm_values.serviceVpcNetworks.ipv4.enabled must be a Boolean"
                .to_string(),
        ));
    }

    // Explicit management requires the chart packaged with Core because predecessor charts neither
    // honor DHCP-server suppression nor render the service-VPC network runtime.
    if platform_helm_version.trim().is_empty() {
        return Err(DpfError::ConfigError(
            "the Core build has no Helm chart version for the IPv4 service-VPC platform release"
                .to_string(),
        ));
    }

    let mut helm_values = dhcp_server_helm_values(cfg);
    // Suppress only the NICo DHCP server after operator overlays so supported DaemonSet placement,
    // metadata, update, and resource settings continue to configure the DHCP CNI daemon.
    let service_daemon_set = helm_values
        .as_object_mut()
        .expect("generated DHCP Helm values must be an object")
        .entry("serviceDaemonSet".to_string())
        .or_insert_with(|| serde_json::json!({}));
    // Preserve the previous fallback for malformed non-object overlays instead of activating them.
    if !service_daemon_set.is_object() {
        *service_daemon_set = serde_json::json!({});
    }
    service_daemon_set["enabled"] = serde_json::json!(false);

    Ok(Some(DetachedDpuServiceDefinition {
        name: PLATFORM_SERVICE_VPC_NAME.to_string(),
        namespace: carbide_dpf::NAMESPACE.to_string(),
        labels: BTreeMap::from([(
            PLATFORM_SERVICE_OWNER_LABEL.to_string(),
            PLATFORM_SERVICE_OWNER_VALUE.to_string(),
        )]),
        helm_chart: DetachedHelmChart {
            repo_url: cfg.helm_repo_url.clone(),
            chart: DHCP_SERVER_SERVICE_HELM_NAME.to_string(),
            version: platform_helm_version.to_string(),
            release_name: PLATFORM_SERVICE_VPC_NAME.to_string(),
            values: helm_values.as_object().map(|values| {
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            }),
        },
        deploy_in_cluster: Some(false),
        security: DetachedDpuServiceSecurity {
            privileged: true,
            spiffe: false,
        },
        service_daemon_set: None,
        service_id: Some(DHCP_SERVER_SERVICE_NAME.to_string()),
        interfaces: None,
    }))
}

/// Converts the stable platform definition into a Kubernetes-like observation so startup tests
/// can verify ownership and reconciliation without constructing a Kubernetes client.
#[cfg(test)]
pub(crate) fn platform_service_observation(
    service: &DetachedDpuServiceDefinition,
) -> DpuServiceObservation {
    DpuServiceObservation {
        // Mirror ownership and immutable release fields so tests change only their named conflict.
        name: Some(service.name.clone()),
        namespace: Some(service.namespace.clone()),
        labels: service.labels.clone(),
        helm_chart: DpuServiceHelmChartObservation {
            repo_url: service.helm_chart.repo_url.clone(),
            chart: Some(service.helm_chart.chart.clone()),
            version: service.helm_chart.version.clone(),
            release_name: Some(service.helm_chart.release_name.clone()),
            values: service.helm_chart.values.clone(),
        },
        // Omit DPU attachment and DPF DaemonSet overrides because Helm owns the CNI daemon manifest.
        deploy_in_cluster: service.deploy_in_cluster,
        dpu_cluster_selector_present: false,
        interfaces: service.interfaces.clone(),
        paused: None,
        security: Some(DpuServiceSecurityObservation {
            privileged: Some(service.security.privileged),
            spiffe: service.security.spiffe,
        }),
        service_daemon_set: service.service_daemon_set.as_ref().map(|daemon_set| {
            DpuServiceDaemonSetObservation {
                node_selector: None,
                annotations: daemon_set.annotations.clone(),
                labels: daemon_set.labels.clone(),
                resources: daemon_set.resources.clone(),
                update_strategy: None,
            }
        }),
        service_id: service.service_id.clone(),
        config_ports_present: false,
        // Provide Kubernetes identifiers used by ownership races and resource-version fencing.
        uid: Some(format!("uid-{}", service.name)),
        resource_version: Some("1".to_string()),
        // Observe an active DPUService so finalization remains an explicit per-test mutation.
        is_deleting: false,
    }
}

/// Validates the stable DPUService identity before Core treats it as the shared IPv4 NAD release.
fn verify_platform_service_vpc_ipv4(
    existing: &DpuServiceObservation,
    desired: &DetachedDpuServiceDefinition,
) -> Result<(), DpfError> {
    // The ownership label is checked first because a deterministic name never authorizes adoption.
    if existing
        .labels
        .get(PLATFORM_SERVICE_OWNER_LABEL)
        .map(String::as_str)
        != Some(PLATFORM_SERVICE_OWNER_VALUE)
    {
        return Err(DpfError::InvalidState(format!(
            "DPUService {} is not owned by the IPv4 service-VPC platform release",
            desired.name
        )));
    }

    // Refuse immutable identity drift so reconciliation cannot repurpose a same-name DPUService.
    let immutable_identity_matches = existing.name.as_deref() == Some(desired.name.as_str())
        && existing.namespace.as_deref() == Some(desired.namespace.as_str())
        && existing.helm_chart.release_name.as_deref()
            == Some(desired.helm_chart.release_name.as_str())
        && existing.deploy_in_cluster == desired.deploy_in_cluster
        && !existing.dpu_cluster_selector_present
        && existing.interfaces.is_none()
        && existing.paused != Some(true)
        && existing
            .security
            .as_ref()
            .is_some_and(|security| security.privileged == Some(true) && !security.spiffe)
        && existing.service_daemon_set.is_none()
        && existing.service_id.as_deref() == desired.service_id.as_deref()
        && !existing.config_ports_present
        && !existing.is_deleting;
    if !immutable_identity_matches {
        return Err(DpfError::InvalidState(format!(
            "DPUService {} has conflicting immutable platform-release fields",
            desired.name
        )));
    }

    Ok(())
}

/// Creates or updates the stable IPv4 network release without adopting a foreign same-name object.
pub(crate) async fn reconcile_platform_service_vpc_ipv4(
    dpf_sdk: &dyn DpfOperations,
    desired: &DetachedDpuServiceDefinition,
) -> Result<(), DpfError> {
    // Read the fixed DPUService before writing so a foreign object is never adopted implicitly.
    let existing = match dpf_sdk.get_dpu_service(&desired.name).await? {
        Some(existing) => existing,
        None => match dpf_sdk.create_dpu_service(desired).await {
            Ok(created) => created,
            // A concurrent creator won the fixed name, so reread and verify its ownership.
            Err(error) if error.is_already_exists_or_conflict() => dpf_sdk
                .get_dpu_service(&desired.name)
                .await?
                .ok_or_else(|| {
                    DpfError::InvalidState(format!(
                        "DPUService {} disappeared after a create conflict",
                        desired.name
                    ))
                })?,
            Err(error) => return Err(error),
        },
    };
    verify_platform_service_vpc_ipv4(&existing, desired)?;

    if dpu_service_mutable_fields_match(desired, &existing) {
        return Ok(());
    }

    // Fence mutable updates with the verified observation so another writer forces a reread.
    let resource_version = existing.resource_version.as_deref().ok_or_else(|| {
        DpfError::InvalidState(format!(
            "DPUService {} is missing metadata.resourceVersion",
            desired.name
        ))
    })?;
    let patch = dpu_service_mutable_patch(desired, &existing, resource_version);
    match dpf_sdk.patch_dpu_service(&desired.name, patch).await {
        Ok(patched) => {
            verify_platform_service_vpc_ipv4(&patched, desired)?;
            if dpu_service_mutable_fields_match(desired, &patched) {
                Ok(())
            } else {
                Err(DpfError::InvalidState(format!(
                    "DPUService {} did not converge after its platform-release update",
                    desired.name
                )))
            }
        }
        // Accept only a concurrent update that converged to the exact desired mutable state.
        Err(error) if error.is_already_exists_or_conflict() => {
            let refreshed = dpf_sdk
                .get_dpu_service(&desired.name)
                .await?
                .ok_or_else(|| {
                    DpfError::InvalidState(format!(
                        "DPUService {} disappeared after an update conflict",
                        desired.name
                    ))
                })?;
            verify_platform_service_vpc_ipv4(&refreshed, desired)?;
            if dpu_service_mutable_fields_match(desired, &refreshed) {
                Ok(())
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

/// Forge FMDS service definition.
///
/// `use_node_tokens` defaults to the API's `[node_auth] enabled` switch and can
/// be overridden by `fmds_use_node_tokens`: when set, fmds is deployed fetching
/// bearer JWTs from the dpu-agent's local API socket instead of mounting the
/// machine cert/key (issue #355). Requires a dpu-agent image that serves the
/// local API.
pub(crate) fn fmds_service(
    cfg: &DpfServiceConfig,
    dpu_interfaces: &[DpuServiceInterfaceTemplateDefinition],
    use_node_tokens: bool,
) -> ServiceDefinition {
    let mut helm_values = serde_json::json!({
        "image": {
            "repository": cfg.docker_repo_url,
            "tag": cfg.docker_image_tag,
        },
        "useNodeTokens": use_node_tokens,
    });
    apply_helm_values(&mut helm_values, cfg);
    reassert_api_owned_value(
        &mut helm_values,
        FMDS_SERVICE_NAME,
        &["useNodeTokens"],
        serde_json::json!(use_node_tokens),
    );
    ServiceDefinition {
        helm_values: Some(helm_values),

        interfaces: fmds_service_interfaces(dpu_interfaces),
        apply_node_effect: Some(false),

        service_daemon_set_annotations: Some(BTreeMap::new()),

        service_nads: vec![ServiceNAD {
            name: FMDS_SERVICE_NAD_NAME.to_string(),
            bridge: Some("br-sfc".to_string()),
            resource_type: ServiceNADResourceType::Sf,
            ipam: Some(false),
            mtu: Some(FMDS_SERVICE_MTU),
        }],

        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

/// OTel service definition.
pub(crate) fn otelcol_service(cfg: &DpfServiceConfig) -> ServiceDefinition {
    let mut helm_values = serde_json::json!({
        "image": {
            "repository": cfg.docker_repo_url,
            "tag": cfg.docker_image_tag,
        }
    });
    apply_helm_values(&mut helm_values, cfg);
    ServiceDefinition {
        helm_values: Some(helm_values),
        apply_node_effect: Some(false),
        service_daemon_set_annotations: Some(BTreeMap::new()),
        config_values: Some(serde_json::json!({
            "nico_dpu_agent": "{{ (index .Services \"carbide-dpu-agent\").Name }}",
            "nico_fmds": "{{ (index .Services \"carbide-fmds\").Name }}",
            "nico_dts": "{{ (index .Services \"dts\").Name }}"
        })),

        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

pub(crate) fn doca_weave_dhcp_agent_service(cfg: &DpfServiceConfig) -> ServiceDefinition {
    let mut helm_values = serde_json::json!({
        "weaveDHCPAgent": {
            "containers": {
                "weaveDHCPAgent": {
                    "image": {
                        "repository": cfg.docker_repo_url,
                        "tag": cfg.docker_image_tag,
                    }
                }
            }
        }
    });
    apply_helm_values(&mut helm_values, cfg);
    ServiceDefinition {
        helm_values: Some(helm_values),
        config_values: Some(serde_json::json!({
            "weaveDHCPAgent": {
                "enabled": true,
                "dhcpNetworks": {
                    "createNADs": true,
                    "networks": weave_dhcp_agent_networks(),
                }
            }
        })),
        service_daemon_set_resources: Some(BTreeMap::from([(
            "nvidia.com/bf_sf".to_string(),
            IntOrString::Int(WEAVE_DHCP_AGENT_NETWORKS.len() as i32),
        )])),
        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

const WEAVE_DHCP_AGENT_NETWORKS: &[&str] = &["A56", "A53", "A6", "A3", "A43", "A46", "A13", "A16"];

fn weave_dhcp_agent_networks() -> Vec<serde_json::Value> {
    WEAVE_DHCP_AGENT_NETWORKS
        .iter()
        .map(|port| {
            serde_json::json!({
                // Kubernetes NAD names must be lowercase.
                "name": format!("dhcp-{}", port.to_ascii_lowercase()),
                "bridge": format!("br-dhcp-{port}"),
                "resourceName": "nvidia.com/bf_sf",
                "interfaceName": port,
            })
        })
        .collect()
}

/// PCI address of each BF4 Astra uplink paired with the switch port it carries.
/// The underlay, overlay, and bridge names are all derived from the port name.
const WEAVE_FLOW_CONTROLLER_UNDERLAY_PORTS: &[(&str, &str)] = &[
    ("0005:06:00.0", "A56"),
    ("0005:03:00.0", "A53"),
    ("0000:06:00.0", "A6"),
    ("0000:03:00.0", "A3"),
    ("0004:03:00.0", "A43"),
    ("0004:06:00.0", "A46"),
    ("0001:03:00.0", "A13"),
    ("0001:06:00.0", "A16"),
];

fn weave_flow_controller_underlay_interfaces() -> Vec<serde_json::Value> {
    WEAVE_FLOW_CONTROLLER_UNDERLAY_PORTS
        .iter()
        .map(|(pci_address, port)| {
            serde_json::json!({
                "pciAddress": pci_address,
                "underlayInterface": format!("brcx-{port}"),
                "overlayDHCPInterface": port,
                "dhcpBridgeName": format!("br-dhcp-{port}"),
                "dropBridgeName": format!("br-drop-{port}"),
            })
        })
        .collect()
}

pub(crate) fn doca_weave_flow_controller_service(
    cfg: &DpfServiceConfig,
    ewethers_config: Option<&EwEthersConfig>,
) -> ServiceDefinition {
    let overlay_network_prefix_len = ewethers_config
        .map(|config| config.subnet_mask)
        .unwrap_or_else(EwEthersConfig::default_subnet_mask);
    let default_astra_config = crate::cfg::file::AstraConfig::default();
    let astra_config = ewethers_config
        .map(|config| &config.astra)
        .unwrap_or(&default_astra_config);
    let underlay_config_map_data = serde_json::json!({
        "nicIDType": "mac",
        "overlayNetworkPrefixLength": overlay_network_prefix_len,
        "softwarePlaneIDBitLength": astra_config.underlay_ip_software_plane_id_bit_len,
        "railIDBitLength": astra_config.underlay_ip_rail_id_bit_len,
        "interfaces": weave_flow_controller_underlay_interfaces(),
    });
    let mut helm_values = serde_json::json!({
        "weaveFlowController": {
            "containers": {
                "weaveFlowController": {
                    "image": {
                        "repository": cfg.docker_repo_url,
                        "tag": cfg.docker_image_tag,
                    }
                }
            }
        }
    });
    apply_helm_values(&mut helm_values, cfg);
    ServiceDefinition {
        helm_values: Some(helm_values),
        config_values: Some(serde_json::json!({
            "weaveFlowController": {
                "enabled": true,
                "underlayConfigMapData": underlay_config_map_data
            }
        })),
        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

pub(crate) fn doca_xplane_service(cfg: &DpfServiceConfig) -> ServiceDefinition {
    let mut helm_values = serde_json::json!({
        "image": {
            "repository": cfg.docker_repo_url,
            "tag": cfg.docker_image_tag,
        }
    });
    apply_helm_values(&mut helm_values, cfg);
    ServiceDefinition {
        helm_values: Some(helm_values),
        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
    }
}

/// Build the full list of resolved mandatory DPU services.
///
/// `node_auth` mirrors the API's `[node_auth]` section: fmds token mode follows
/// `enabled` unless `fmds_use_node_tokens` overrides it (issue #355).
pub(crate) fn mandatory_services(
    resolved: &DpfResolvedMandatoryServicesConfig,
    bootstrap_ca: &DpfDpuAgentBootstrapCa,
    interfaces: &[DpuServiceInterfaceTemplateDefinition],
    service_vpc_slots: ServiceVpcSlots,
    node_auth: &NodeAuthConfig,
    ewethers_config: Option<&EwEthersConfig>,
) -> Vec<ServiceDefinition> {
    // Slot listeners attach directly to their own bridge, so each needs its own NAD.
    let mut dhcp = dhcp_server_service(&resolved.base.dhcp_server, interfaces);
    service_vpc_slots.append_dhcp_interfaces(&mut dhcp);

    // If xplane service is configured then set the xplaneMaintenance so that the dpu
    // agent can mount socket and other paths associated with xplane.
    // Apply this after extra_helm_values so an override cannot remove those mounts.
    let mut agent = dpu_agent_service(&resolved.base.dpu_agent, bootstrap_ca);
    if resolved.extra.contains_key(&DpfExtraService::DocaXplane) {
        agent
            .helm_values
            .as_mut()
            .expect("DPU agent has generated Helm values")["xplaneMaintenance"] =
            serde_json::json!({ "enabled": true });
    }
    let mut service_vec = vec![
        dts_service(&resolved.base.dts),
        doca_hbn_service(&resolved.base.doca_hbn, interfaces, service_vpc_slots),
        dhcp,
        agent,
        // Not `node_auth.enabled` directly: an operator staging a disable
        // moves fmds off tokens first, while the API still accepts them.
        fmds_service(
            &resolved.base.fmds,
            interfaces,
            node_auth.fmds_use_node_tokens(),
        ),
        otelcol_service(&resolved.base.otel),
    ];

    for (service, cfg) in &resolved.extra {
        match service {
            DpfExtraService::DocaWeaveDhcpAgent => {
                service_vec.push(doca_weave_dhcp_agent_service(cfg))
            }
            DpfExtraService::DocaWeaveFlowController => {
                service_vec.push(doca_weave_flow_controller_service(cfg, ewethers_config))
            }
            DpfExtraService::DocaXplane => service_vec.push(doca_xplane_service(cfg)),
        }
    }

    service_vec
}

#[cfg(test)]
mod tests {
    use carbide_dpf::sdk::{build_dpu_interfaces_vec, build_effective_dpu_interfaces};
    use carbide_dpf::types::{
        DpfInterceptBridge, DpfInterceptBridging, DpfInterfaceIdentity, DpuDeploymentType,
        DpuServiceInterfaceTemplateType,
    };
    use carbide_dpf::{
        SERVICE_VPC_MTU, build_deployment_dpu_interfaces, build_service_configuration,
        build_service_interface, build_service_nad, build_service_template,
    };
    use carbide_machine_controller::dpf::MockDpfOperations;
    use carbide_test_support::value_scenarios;
    use url::Url;

    use super::*;

    const TEST_NS: &str = "dpf-operator-system";
    const TEST_PLATFORM_HELM_VERSION: &str = "test-platform-version";

    /// Builds an explicitly managed platform DPUService so reconciliation tests exercise the
    /// stable-release contract without depending on build-time Helm metadata.
    fn managed_platform_service(enabled: bool) -> DetachedDpuServiceDefinition {
        // Supply the explicit control value that distinguishes managed releases from legacy config.
        let mut config = default_dhcp_server_service();
        config.extra_helm_values = serde_json::json!({
            "serviceVpcNetworks": {
                "ipv4": {
                    "enabled": enabled,
                }
            }
        })
        .as_object()
        .cloned();

        // Use fixed chart metadata so these tests isolate reconciliation behavior.
        platform_service_vpc_ipv4(&config, TEST_PLATFORM_HELM_VERSION)
            .expect("managed platform configuration must be valid")
            .expect("an explicit control value must produce a platform release")
    }

    /// Verifies every valid IPv4-control omission bypasses packaged-chart validation and preserves
    /// the network values supplied to DPUDeployment releases, so an unmanaged upgrade introduces no
    /// platform settings.
    #[test]
    fn platform_service_vpc_ipv4_omission_preserves_legacy_configuration() {
        value_scenarios!(
            run = |extra_helm_values: Option<serde_json::Map<String, serde_json::Value>>| {
                // Retain the exact unmanaged network value for comparison with deployment output.
                let expected_networks = extra_helm_values
                    .as_ref()
                    .and_then(|values| values.get("serviceVpcNetworks"))
                    .cloned();
                // Simulate a build without Helm metadata so omission must bypass version validation.
                let mut config = default_dhcp_server_service();
                config.extra_helm_values = extra_helm_values;
                let platform_is_unmanaged =
                    matches!(platform_service_vpc_ipv4(&config, ""), Ok(None));
                let deployment = dhcp_server_service(&config, &[]);
                let actual_networks = deployment
                    .helm_values
                    .as_ref()
                    .and_then(|values| values.get("serviceVpcNetworks"))
                    .cloned();

                platform_is_unmanaged && actual_networks == expected_networks
            };
            "valid omission preserves unmanaged configurations" {
                // Preserve legacy configuration when the complete network block is absent.
                None => true,
                // Preserve an IPv6-only overlay because it does not opt into IPv4 platform
                // management.
                Some(
                    serde_json::json!({
                        "serviceVpcNetworks": {
                            "ipv6": {
                                "enabled": true,
                            }
                        }
                    })
                    .as_object()
                    .unwrap()
                    .clone()
                ) => true,
                // Preserve partial IPv4 values until the Boolean enabled control is supplied.
                Some(
                    serde_json::json!({
                        "serviceVpcNetworks": {
                            "ipv4": {
                                "name": SERVICE_VPC_IPV4_NAD_NAME,
                            }
                        }
                    })
                    .as_object()
                    .unwrap()
                    .clone()
                ) => true,
            }
        );
    }

    /// Verifies explicit management requires packaged chart metadata so Core cannot submit an
    /// invalid empty chart version to the DPUService CRD.
    #[test]
    fn platform_service_vpc_ipv4_rejects_missing_packaged_chart_version() {
        // Explicitly enable platform management while simulating a build without Helm metadata.
        let mut config = default_dhcp_server_service();
        config.extra_helm_values = serde_json::json!({
            "serviceVpcNetworks": {
                "ipv4": {
                    "enabled": true,
                }
            }
        })
        .as_object()
        .cloned();

        // Reject the configuration before any Kubernetes reconciliation can begin.
        let error = platform_service_vpc_ipv4(&config, "")
            .expect_err("managed platform releases require a packaged chart version");
        assert!(matches!(error, DpfError::ConfigError(_)));
    }

    /// Verifies malformed management paths fail locally because treating them as omission could
    /// silently leave a previously enabled platform NAD unchanged.
    #[test]
    fn platform_service_vpc_ipv4_rejects_malformed_management_values() {
        /// Carries one malformed Helm overlay and the diagnostic path it must identify.
        struct MalformedValue {
            extra_helm_values: serde_json::Value,
            expected_path: &'static str,
        }

        value_scenarios!(
            run = |MalformedValue {
                extra_helm_values,
                expected_path,
            }| {
                // Keep unrelated service fields valid so each row isolates one malformed control.
                let mut config = default_dhcp_server_service();
                config.extra_helm_values = Some(
                    extra_helm_values
                        .as_object()
                        .expect("test Helm values must have an object root")
                        .clone(),
                );

                // Require the diagnostic to identify the malformed configuration path.
                matches!(
                    platform_service_vpc_ipv4(&config, TEST_PLATFORM_HELM_VERSION),
                    Err(DpfError::ConfigError(message)) if message.contains(expected_path)
                )
            };
            "malformed management values identify their configuration path" {
                // Reject a scalar network block because it cannot contain the IPv4 control.
                MalformedValue {
                    extra_helm_values: serde_json::json!({ "serviceVpcNetworks": true }),
                    expected_path: "serviceVpcNetworks",
                } => true,
                // Reject a scalar IPv4 block because it cannot contain the enablement control.
                MalformedValue {
                    extra_helm_values: serde_json::json!({
                        "serviceVpcNetworks": {
                            "ipv4": true,
                        }
                    }),
                    expected_path: "serviceVpcNetworks.ipv4",
                } => true,
                // Reject a non-Boolean control because truthy coercion would make IPv4 management
                // ambiguous.
                MalformedValue {
                    extra_helm_values: serde_json::json!({
                        "serviceVpcNetworks": {
                            "ipv4": {
                                "enabled": "true",
                            }
                        }
                    }),
                    expected_path: "serviceVpcNetworks.ipv4.enabled",
                } => true,
            }
        );
    }

    /// Pins the fixed platform release identity plus its placement, security, service identity, and
    /// interface fields because Argo ownership and DPF validation depend on exact CR literals.
    #[test]
    fn platform_service_vpc_ipv4_has_fixed_cr_contract() {
        // Construct the managed release through the same builder startup uses.
        let platform = managed_platform_service(true);

        // Assert the stable identity and exact DPF placement, security, and attachment fields.
        assert_eq!(platform.name, "nico-service-vpc");
        assert_eq!(platform.namespace, "dpf-operator-system");
        assert_eq!(
            platform.labels,
            BTreeMap::from([(
                "carbide.nvidia.com/platform-service".to_string(),
                "service-vpc".to_string(),
            )])
        );
        assert_eq!(platform.helm_chart.chart, "nico-dhcp-server");
        assert_eq!(platform.helm_chart.release_name, "nico-service-vpc");
        assert_eq!(platform.deploy_in_cluster, Some(false));
        assert!(platform.security.privileged);
        assert!(!platform.security.spiffe);
        assert!(platform.service_daemon_set.is_none());
        assert_eq!(platform.service_id.as_deref(), Some("carbide-dhcp-server"));
        assert!(platform.interfaces.is_none());
    }

    /// Verifies lifecycle, placement, DPF workload overrides, ports, security, and service-identity
    /// drift cannot replace the chart-managed network release that Core constructed.
    #[test]
    fn platform_service_vpc_ipv4_verifier_rejects_forbidden_runtime_state() {
        // Start from a complete owned observation so each mutation isolates one forbidden state.
        let desired = managed_platform_service(false);
        let matching = platform_service_observation(&desired);

        // Mark the otherwise matching DPUService as deleting to isolate lifecycle drift.
        let mut deleting = matching.clone();
        deleting.is_deleting = true;

        // Add a DPU cluster selector to isolate placement drift from every other field.
        let mut selected = matching.clone();
        selected.dpu_cluster_selector_present = true;

        // Pause the otherwise matching DPUService to isolate reconciliation state drift.
        let mut paused = matching.clone();
        paused.paused = Some(true);

        // Attach one service interface to isolate DPU attachment drift.
        let mut attached = matching.clone();
        attached.interfaces = Some(vec!["unexpected-interface".to_string()]);

        // Add config ports to isolate generated Service configuration drift.
        let mut configured_ports = matching.clone();
        configured_ports.config_ports_present = true;

        // Remove privilege to isolate the permission required by the DHCP CNI daemon.
        let mut unprivileged = matching.clone();
        unprivileged
            .security
            .as_mut()
            .expect("matching platform observation has security")
            .privileged = Some(false);

        // Enable SPIFFE to isolate workload identity drift.
        let mut spiffe_enabled = matching.clone();
        spiffe_enabled
            .security
            .as_mut()
            .expect("matching platform observation has security")
            .spiffe = true;

        // Replace the service ID to isolate DPF-generated identity drift.
        let mut changed_service_id = matching.clone();
        changed_service_id.service_id = Some("another-service".to_string());

        // Add a DPF DaemonSet override to isolate drift from the chart-owned daemon manifest.
        let mut daemon_set_override = matching;
        daemon_set_override.service_daemon_set = Some(DpuServiceDaemonSetObservation {
            node_selector: None,
            annotations: None,
            labels: None,
            resources: None,
            update_strategy: None,
        });

        value_scenarios!(
            run = |observation| matches!(
                verify_platform_service_vpc_ipv4(&observation, &desired),
                Err(DpfError::InvalidState(_))
            );
            "forbidden platform-service state is rejected" {
                // Reject a finalizer-held object because it cannot provide a durable stable release.
                deleting => true,
                // Reject a DPU cluster selector because it changes where the fixed release deploys.
                selected => true,
                // Reject a paused release because it cannot apply or prune the shared NAD.
                paused => true,
                // Reject attached interfaces because the platform release has no DPF attachments.
                attached => true,
                // Reject config ports because the platform release exposes no Kubernetes Service.
                configured_ports => true,
                // Reject missing privilege because DPF would deny the DHCP CNI daemon.
                unprivileged => true,
                // Reject SPIFFE because the platform runtime needs no workload identity.
                spiffe_enabled => true,
                // Reject another service ID because it changes DPF's generated service identity.
                changed_service_id => true,
                // Reject DPF DaemonSet settings because the chart owns the runtime manifest.
                daemon_set_override => true,
            }
        );
    }

    /// Verifies the stable platform release preserves the configured repository and sibling
    /// DaemonSet settings, uses Core's packaged chart, and suppresses only the NICo DHCP server
    /// while deployment DHCP remains pinned, preventing mirror bypass, deployment drift, and a
    /// second server without discarding supported DHCP CNI daemon configuration.
    #[test]
    fn platform_service_vpc_ipv4_uses_packaged_chart_and_suppresses_workload_overlay() {
        // Route both releases through a nondefault mirror, pin the deployment chart, and attempt to
        // enable the stable NAD and DHCP server while supplying one sibling DaemonSet setting.
        let mut config = default_dhcp_server_service();
        config.helm_repo_url = "oci://mirror.example.test/nico".to_string();
        config.helm_chart = "legacy-dhcp-server".to_string();
        config.helm_version = "legacy-version".to_string();
        config.extra_helm_values = serde_json::json!({
            "serviceDaemonSet": {
                "enabled": true,
                "annotations": {
                    "example.com/platform": "retained",
                },
            },
            "serviceVpcNetworks": {
                "ipv4": {
                    "enabled": true,
                }
            }
        })
        .as_object()
        .cloned();

        // Build both releases from the same top-level service configuration.
        let deployment = dhcp_server_service(&config, &[]);
        let platform = platform_service_vpc_ipv4(&config, TEST_PLATFORM_HELM_VERSION)
            .expect("platform configuration must be valid")
            .expect("explicit enablement must produce a platform release");

        // Deployment pinning remains intact while the platform release preserves the mirror, uses
        // the supported chart, retains the DaemonSet annotation, and forces only the DHCP server off.
        assert_eq!(deployment.helm_chart, "legacy-dhcp-server");
        assert_eq!(deployment.helm_version, "legacy-version");
        assert_eq!(platform.helm_chart.repo_url, config.helm_repo_url);
        assert_eq!(platform.helm_chart.chart, DHCP_SERVER_SERVICE_HELM_NAME);
        assert_eq!(platform.helm_chart.version, TEST_PLATFORM_HELM_VERSION);
        let platform_values = platform
            .helm_chart
            .values
            .expect("stable platform Helm values");
        assert_eq!(platform_values["serviceDaemonSet"]["enabled"], false);
        assert_eq!(
            platform_values["serviceDaemonSet"]["annotations"]["example.com/platform"],
            "retained"
        );
    }

    /// Verifies top-level NAD enablement reaches only the stable release because allowing BF3 and
    /// its GB200 derivative to inherit it would give multiple Argo Applications the fixed name.
    #[test]
    fn service_vpc_ipv4_has_one_stable_owner_across_bf3_variants() {
        // Enable the NAD in the top-level DHCP values that both BF3 variants otherwise inherit.
        let mut config = crate::cfg::file::DpfConfig::default();
        config.services.dhcp_server.extra_helm_values = serde_json::json!({
            "serviceVpcNetworks": {
                "ipv4": {
                    "enabled": true,
                    "name": SERVICE_VPC_IPV4_NAD_NAME,
                    "mtu": SERVICE_VPC_MTU,
                }
            }
        })
        .as_object()
        .cloned();

        // Build the stable release and both deployment-owned DHCP definitions through Core.
        let platform = platform_service_vpc_ipv4(
            &config.resolved_mandatory_services().dhcp_server,
            TEST_PLATFORM_HELM_VERSION,
        )
        .expect("platform configuration must be valid")
        .expect("explicit enablement must produce a platform release");
        let bf3_services =
            config.resolved_services_for(&config.deployments.bf3, DpuDeploymentType::Bf3);
        let bf3_gb200 = config.deployments.bf3.bf3_gb200();
        let bf3_gb200_services =
            config.resolved_services_for(&bf3_gb200, DpuDeploymentType::Bf3Gb200);
        let bf3 = dhcp_server_service(&bf3_services.base.dhcp_server, &[]);
        let gb200 = dhcp_server_service(&bf3_gb200_services.base.dhcp_server, &[]);

        // The stable release retains explicit IPv4 enablement while suppressing the NICo DHCP
        // server.
        let platform_values = platform
            .helm_chart
            .values
            .expect("stable platform Helm values");
        assert_eq!(
            platform_values["serviceVpcNetworks"]["ipv4"]["enabled"],
            true
        );
        assert_eq!(platform_values["serviceDaemonSet"]["enabled"], false);
        // Each deployment-owned release disables the IPv4 enablement switch while preserving its
        // sibling fixed-name and MTU values.
        for deployment_values in [
            // The base BF3 deployment must never render the shared platform NAD.
            bf3.helm_values.expect("BF3 DHCP Helm values"),
            // The derived GB200 deployment must not inherit top-level NAD enablement.
            gb200.helm_values.expect("BF3 GB200 DHCP Helm values"),
        ] {
            assert_eq!(
                deployment_values["serviceVpcNetworks"]["ipv4"]["enabled"],
                false
            );
            assert_eq!(
                deployment_values["serviceVpcNetworks"]["ipv4"]["name"],
                SERVICE_VPC_IPV4_NAD_NAME
            );
            assert_eq!(
                deployment_values["serviceVpcNetworks"]["ipv4"]["mtu"],
                SERVICE_VPC_MTU
            );
        }
    }

    /// Verifies a deployment-local mandatory-service replacement cannot manage or render the
    /// site-wide IPv4 NAD because only the top-level DHCP configuration can create the stable
    /// platform release.
    #[test]
    fn deployment_service_replacement_cannot_own_platform_service_vpc_ipv4() {
        // Put the only enablement in BF3's mandatory-service replacement; the cloned defaults keep
        // every unrelated mandatory service valid.
        let mut config = crate::cfg::file::DpfConfig::default();
        let mut deployment_services = (*config.services).clone();
        deployment_services.dhcp_server.extra_helm_values = serde_json::json!({
            "serviceVpcNetworks": {
                "ipv4": {
                    "enabled": true,
                    "name": SERVICE_VPC_IPV4_NAD_NAME,
                    "mtu": SERVICE_VPC_MTU,
                }
            }
        })
        .as_object()
        .cloned();
        config.deployments.bf3.services = Some(Box::new(deployment_services));

        // Resolve the top-level platform release and both deployment releases fed by the BF3
        // replacement.
        let platform = platform_service_vpc_ipv4(
            &config.resolved_mandatory_services().dhcp_server,
            TEST_PLATFORM_HELM_VERSION,
        )
        .expect("top-level platform configuration must remain valid");
        let bf3_services =
            config.resolved_services_for(&config.deployments.bf3, DpuDeploymentType::Bf3);
        let bf3_gb200 = config.deployments.bf3.bf3_gb200();
        let bf3_gb200_services =
            config.resolved_services_for(&bf3_gb200, DpuDeploymentType::Bf3Gb200);
        let bf3 = dhcp_server_service(&bf3_services.base.dhcp_server, &[]);
        let gb200 = dhcp_server_service(&bf3_gb200_services.base.dhcp_server, &[]);

        // Local enablement must neither create the platform release nor survive deployment
        // rendering.
        assert!(platform.is_none());
        for deployment_values in [
            // Strip enablement from BF3 because its local replacement cannot configure the
            // site-wide release.
            bf3.helm_values.expect("BF3 DHCP Helm values"),
            // Strip it from GB200 separately because the derived deployment inherits BF3's replacement.
            gb200.helm_values.expect("BF3 GB200 DHCP Helm values"),
        ] {
            assert_eq!(
                deployment_values["serviceVpcNetworks"]["ipv4"]["enabled"],
                false
            );
        }
    }

    /// Verifies startup creates the stable platform release when absent because its DPUService must
    /// exist before Argo can add or later prune the shared NAD.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_creates_missing_owner() {
        // Return no observation and mirror the requested definition from the create operation.
        let desired = managed_platform_service(false);
        let mut dpf_sdk = MockDpfOperations::new();
        dpf_sdk
            .expect_get_dpu_service()
            .times(1)
            .returning(|_| Ok(None));
        dpf_sdk
            .expect_create_dpu_service()
            .times(1)
            .returning(|service| Ok(platform_service_observation(service)));
        dpf_sdk.expect_patch_dpu_service().times(0);
        dpf_sdk.expect_delete_dpu_service().times(0);

        // Reconcile the absent stable release.
        reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired)
            .await
            .expect("missing platform release must be created");
    }

    /// Verifies a non-conflict create failure is returned unchanged because startup must not
    /// continue without the stable platform release when DPF cannot complete its creation.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_propagates_non_conflict_create_failure() {
        // Leave the stable platform release absent and inject a non-conflict failure from its
        // create.
        let desired = managed_platform_service(false);
        let mut dpf_sdk = MockDpfOperations::new();
        dpf_sdk
            .expect_get_dpu_service()
            .times(1)
            .returning(|_| Ok(None));
        dpf_sdk.expect_create_dpu_service().times(1).returning(|_| {
            Err(DpfError::timeout(
                "create DPUService",
                "injected platform create failure",
            ))
        });
        dpf_sdk.expect_patch_dpu_service().times(0);
        dpf_sdk.expect_delete_dpu_service().times(0);

        // Reconcile the missing stable platform release through the failed create.
        let error = reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired)
            .await
            .expect_err("non-conflict create failure must stop reconciliation");

        // Preserve the write error rather than rereading it as a create conflict.
        match error {
            DpfError::Timeout { operation, details } => {
                assert_eq!(operation, "create DPUService");
                assert_eq!(details, "injected platform create failure");
            }
            other => panic!("expected create timeout, got {other:?}"),
        }
    }

    /// Verifies create races accept only a present owned winner so a name collision cannot transfer
    /// the stable release or let startup continue after the winner disappears.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_verifies_create_race_winner() {
        enum Expected {
            Accepted,
            InvalidState,
        }

        // Build owned and foreign winners with otherwise identical platform-release fields.
        let desired = managed_platform_service(false);
        let mut foreign = platform_service_observation(&desired);
        foreign.labels.insert(
            PLATFORM_SERVICE_OWNER_LABEL.to_string(),
            "another-controller".to_string(),
        );

        for (scenario, race_winner, expected) in [
            // Accept another Core replica's object because it carries the ownership contract.
            (
                "owned winner",
                Some(platform_service_observation(&desired)),
                Expected::Accepted,
            ),
            // Reject a foreign winner because the failed create does not authorize adoption.
            ("foreign winner", Some(foreign), Expected::InvalidState),
            // Reject a vanished winner because startup still lacks the required stable owner.
            ("winner disappeared", None, Expected::InvalidState),
        ] {
            // Return absence first, then expose the object that won the concurrent create.
            let mut observations = [None, race_winner].into_iter();
            let conflict_name = desired.name.clone();
            let mut dpf_sdk = MockDpfOperations::new();
            dpf_sdk
                .expect_get_dpu_service()
                .times(2)
                .returning(move |_| {
                    Ok(observations
                        .next()
                        .expect("create race requires two DPUService reads"))
                });
            dpf_sdk
                .expect_create_dpu_service()
                .times(1)
                .return_once(move |_| Err(DpfError::already_exists("DPUService", conflict_name)));
            dpf_sdk.expect_patch_dpu_service().times(0);
            dpf_sdk.expect_delete_dpu_service().times(0);

            // Reconcile through the conflict and check the ownership-specific outcome.
            let result = reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired).await;
            match (expected, result) {
                (Expected::Accepted, Ok(())) => {}
                (Expected::InvalidState, Err(DpfError::InvalidState(_))) => {}
                (Expected::Accepted, Err(error)) => {
                    panic!("{scenario}: unexpected error: {error}")
                }
                (Expected::InvalidState, other) => {
                    panic!("{scenario}: expected InvalidState, got {other:?}")
                }
            }
        }
    }

    /// Verifies startup rejects a foreign same-name DPUService without any mutation because a
    /// deterministic Kubernetes name is not proof that Core owns the release.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_refuses_foreign_owner() {
        // Return the desired immutable fields under a foreign ownership label.
        let desired = managed_platform_service(false);
        let mut foreign = platform_service_observation(&desired);
        foreign.labels.insert(
            PLATFORM_SERVICE_OWNER_LABEL.to_string(),
            "another-controller".to_string(),
        );
        let mut dpf_sdk = MockDpfOperations::new();
        dpf_sdk
            .expect_get_dpu_service()
            .times(1)
            .return_once(move |_| Ok(Some(foreign)));
        dpf_sdk.expect_create_dpu_service().times(0);
        dpf_sdk.expect_patch_dpu_service().times(0);
        dpf_sdk.expect_delete_dpu_service().times(0);

        // Reject the collision before any create, patch, or delete can adopt the object.
        let error = reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired)
            .await
            .expect_err("foreign platform release must be rejected");
        assert!(matches!(error, DpfError::InvalidState(_)));
    }

    /// Verifies an owned DPUService with another Helm release name is rejected because the stable
    /// Kubernetes name must not authorize repurposing a different platform release.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_refuses_immutable_identity_drift() {
        // Retain Core's ownership label while changing the immutable Helm release identity.
        let desired = managed_platform_service(false);
        let mut drifted = platform_service_observation(&desired);
        drifted.helm_chart.release_name = Some("another-platform-release".to_string());
        let mut dpf_sdk = MockDpfOperations::new();
        dpf_sdk
            .expect_get_dpu_service()
            .times(1)
            .return_once(move |_| Ok(Some(drifted)));
        dpf_sdk.expect_create_dpu_service().times(0);
        dpf_sdk.expect_patch_dpu_service().times(0);
        dpf_sdk.expect_delete_dpu_service().times(0);

        // Reject the identity drift before any mutation can repurpose the existing DPUService.
        let error = reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired)
            .await
            .expect_err("immutable platform identity drift must be rejected");
        assert!(matches!(error, DpfError::InvalidState(_)));
    }

    /// Verifies disabling IPv4 patches the existing stable release in place so Argo prunes the
    /// NAD without transferring ownership to a new DPUService.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_patches_enabled_release_to_disabled() {
        // Observe the same owned release with the previously enabled NAD value.
        let desired = managed_platform_service(false);
        let mut enabled = platform_service_observation(&desired);
        enabled
            .helm_chart
            .values
            .as_mut()
            .expect("observed Helm values")
            .insert(
                "serviceVpcNetworks".to_string(),
                serde_json::json!({
                    "ipv4": {
                        "enabled": true,
                        "name": SERVICE_VPC_IPV4_NAD_NAME,
                        "mtu": SERVICE_VPC_MTU,
                    }
                }),
            );
        // Use a nondefault version so the submitted patch must fence the observed generation.
        enabled.resource_version = Some("17".to_string());
        let patched = platform_service_observation(&desired);
        let expected_name = desired.name.clone();
        let mut dpf_sdk = MockDpfOperations::new();
        dpf_sdk
            .expect_get_dpu_service()
            .times(1)
            .return_once(move |_| Ok(Some(enabled)));
        dpf_sdk.expect_create_dpu_service().times(0);
        dpf_sdk
            .expect_patch_dpu_service()
            .times(1)
            .withf(move |name, patch| {
                // Inspect the operations directly so the test does not reuse the patch builder.
                let Some(operations) = patch.as_array() else {
                    return false;
                };
                let fences_observed_version = operations.iter().any(|operation| {
                    operation.get("op").and_then(|value| value.as_str()) == Some("replace")
                        && operation.get("path").and_then(|value| value.as_str())
                            == Some("/metadata/resourceVersion")
                        && operation.get("value").and_then(|value| value.as_str()) == Some("17")
                });
                let disables_ipv4 = operations.iter().any(|operation| {
                    operation.get("op").and_then(|value| value.as_str()) == Some("add")
                        && operation.get("path").and_then(|value| value.as_str())
                            == Some("/spec/helmChart/values")
                        && operation
                            .pointer("/value/serviceVpcNetworks/ipv4/enabled")
                            .and_then(|value| value.as_bool())
                            == Some(false)
                });
                name == expected_name.as_str() && fences_observed_version && disables_ipv4
            })
            .return_once(move |_, _| Ok(patched));
        dpf_sdk.expect_delete_dpu_service().times(0);

        // Patch the stable release using the observed resource-version fence.
        reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired)
            .await
            .expect("owned platform release must update in place");
    }

    /// Verifies a non-conflict patch failure is returned unchanged because startup must not accept
    /// a stale owned release when its required update did not complete.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_propagates_non_conflict_patch_failure() {
        // Return an owned release with mutable chart drift so reconciliation must patch it.
        let desired = managed_platform_service(false);
        let mut stale = platform_service_observation(&desired);
        stale.helm_chart.version = "previous-version".to_string();
        let mut dpf_sdk = MockDpfOperations::new();
        dpf_sdk
            .expect_get_dpu_service()
            .times(1)
            .return_once(move |_| Ok(Some(stale)));
        dpf_sdk.expect_create_dpu_service().times(0);
        dpf_sdk
            .expect_patch_dpu_service()
            .times(1)
            .returning(|_, _| {
                Err(DpfError::timeout(
                    "patch DPUService",
                    "injected platform patch failure",
                ))
            });
        dpf_sdk.expect_delete_dpu_service().times(0);

        // Reconcile the mutable drift through the failed patch.
        let error = reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired)
            .await
            .expect_err("non-conflict patch failure must stop reconciliation");

        // Preserve the write error without performing the conflict-only reread.
        match error {
            DpfError::Timeout { operation, details } => {
                assert_eq!(operation, "patch DPUService");
                assert_eq!(details, "injected platform patch failure");
            }
            other => panic!("expected patch timeout, got {other:?}"),
        }
    }

    /// Verifies a patch conflict succeeds only when the winning write remains present, owned, and
    /// converged, preventing startup from accepting an absent, stale, or foreign platform release.
    #[tokio::test]
    async fn platform_service_vpc_ipv4_verifies_patch_conflict_reread() {
        enum Expected {
            Accepted,
            InvalidState,
            PatchConflict,
        }

        // Build the distinct reread states after another writer wins the fenced update.
        let desired = managed_platform_service(false);
        let converged = platform_service_observation(&desired);
        // Preserve the prior chart version to model a winner that did not apply Core's patch.
        let mut still_drifted = platform_service_observation(&desired);
        still_drifted.helm_chart.version = "previous-version".to_string();
        // Change only the owner label to isolate foreign ownership after the conflicting write.
        let mut foreign = platform_service_observation(&desired);
        foreign.labels.insert(
            PLATFORM_SERVICE_OWNER_LABEL.to_string(),
            "another-controller".to_string(),
        );

        for (scenario, refreshed, expected) in [
            // Accept a winner that completed the exact desired mutable update.
            ("converged winner", Some(converged), Expected::Accepted),
            // Preserve the conflict when the winning write leaves mutable drift.
            (
                "still-drifted winner",
                Some(still_drifted),
                Expected::PatchConflict,
            ),
            // Reject a winner that replaced the fixed name under another controller's ownership.
            ("foreign winner", Some(foreign), Expected::InvalidState),
            // Reject disappearance because no stable owner remains after the conflicting update.
            ("winner disappeared", None, Expected::InvalidState),
        ] {
            // Start from mutable chart drift, then return the case-specific fresh observation.
            let mut stale = platform_service_observation(&desired);
            stale.helm_chart.version = "previous-version".to_string();
            let mut observations = [Some(stale), refreshed].into_iter();
            let conflict_name = desired.name.clone();
            let expected_name = desired.name.clone();
            let mut dpf_sdk = MockDpfOperations::new();
            dpf_sdk
                .expect_get_dpu_service()
                .times(2)
                .returning(move |_| {
                    Ok(observations
                        .next()
                        .expect("patch conflict requires two DPUService reads"))
                });
            dpf_sdk.expect_create_dpu_service().times(0);
            dpf_sdk
                .expect_patch_dpu_service()
                .times(1)
                .return_once(move |_, _| {
                    Err(DpfError::PatchConflict {
                        kind: "DPUService",
                        name: conflict_name,
                    })
                });
            dpf_sdk.expect_delete_dpu_service().times(0);

            // Reconcile through the conflict and check the refreshed mutable state.
            let result = reconcile_platform_service_vpc_ipv4(&dpf_sdk, &desired).await;
            match (expected, result) {
                (Expected::Accepted, Ok(())) => {}
                (Expected::InvalidState, Err(DpfError::InvalidState(_))) => {}
                (
                    Expected::PatchConflict,
                    Err(DpfError::PatchConflict {
                        kind: "DPUService",
                        name,
                    }),
                ) => assert_eq!(name, expected_name, "{scenario}"),
                (Expected::Accepted, Err(error)) => {
                    panic!("{scenario}: unexpected error: {error}")
                }
                (Expected::InvalidState, other) => {
                    panic!("{scenario}: expected InvalidState, got {other:?}")
                }
                (Expected::PatchConflict, other) => {
                    panic!("{scenario}: expected PatchConflict, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn service_upgrade_policies() {
        let interfaces = build_dpu_interfaces_vec();
        let bootstrap_ca = DpfDpuAgentBootstrapCa::default();
        value_scenarios!(
            run = |service: ServiceDefinition| {
                build_service_configuration(&service, TEST_NS, "", &BTreeMap::new())
                    .spec.upgrade_policy.apply_node_effect
            };
            "NICo services update without NodeEffect" {
                dpu_agent_service(&default_dpu_agent_service(), &bootstrap_ca) => Some(false),
                dhcp_server_service(&default_dhcp_server_service(), &interfaces) => Some(false),
                fmds_service(&default_fmds_service(), &interfaces, false) => Some(false),
                otelcol_service(&default_otelcol_service()) => Some(false),
            }
            "other services retain the NodeEffect gate" {
                doca_hbn_service(
                    &default_doca_hbn_service(),
                    &interfaces,
                    ServiceVpcSlots::default(),
                ) => Some(true),
                dts_service(&default_dts_service()) => Some(true),
                ServiceDefinition {
                    apply_node_effect: Some(true),
                    ..dts_service(&default_dts_service())
                } => Some(true),
            }
        );
    }

    /// HBN configuration follows the platform inventory used to build service chains.
    #[test]
    fn default_hbn_omits_hidden_bf3_host_pf1() {
        for (deployment_type, has_pf1) in [
            (DpuDeploymentType::Bf3, false),
            (DpuDeploymentType::Bf3Gb200, false),
            (DpuDeploymentType::Bf4Generic, true),
        ] {
            let interfaces = build_deployment_dpu_interfaces(deployment_type, 16, None);
            let hbn = doca_hbn_service(
                &default_doca_hbn_service(),
                &interfaces,
                ServiceVpcSlots::default(),
            );
            assert_eq!(
                hbn.interfaces
                    .iter()
                    .any(|interface| interface.name == "pf1hpf_if"),
                has_pf1
            );
            assert!(
                hbn.interfaces
                    .iter()
                    .any(|interface| interface.name == "p1_if")
            );
            let startup_yaml =
                hbn.config_values.as_ref().unwrap()["configuration"]["startupYAMLJ2"]
                    .as_str()
                    .unwrap();
            assert_eq!(startup_yaml.contains("pf1hpf_if:"), has_pf1);
        }
    }

    /// Verifies every service definition consumes the same configured effective inventory.
    /// Mandatory-service assembly must pass slots to DHCP as well as HBN to avoid listener gaps.
    /// Zero slots preserve ordinary NAD references so rollout retains existing BF3 attachments.
    #[test]
    fn configured_inventory_drives_hbn_dhcp_and_fmds_definitions() {
        // Build a complete replacement inventory containing one PF and one VF.
        let topology = DpfInterceptBridging::new(
            vec![
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: None,
                    },
                    "br-pf3",
                    "p-pf3",
                ),
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: Some(4),
                    },
                    "br-vf4",
                    "p-vf4",
                ),
            ],
            16,
        )
        .expect("configured service inventory fixture must be valid");
        let interfaces = build_effective_dpu_interfaces(16, Some(&topology));

        let suffix = carbide_dpf::sdk::deployment_cr_suffix(DpuDeploymentType::Bf3);
        for slot_count in [
            // Disabled slots retain the ordinary DHCP/FMDS attachments and no fixed listeners.
            0,
            // A positive slot extends the same assembled inventory with a bridge-specific listener.
            1,
        ] {
            // Validate the actual mandatory-service list with its matching inventory and topology.
            let service_vpc_slots =
                ServiceVpcSlots::new(slot_count).expect("bounded test slot count");
            let config = crate::cfg::file::DpfConfig::default();
            let services = mandatory_services(
                &config.resolved_services_for(&config.deployments.bf3, DpuDeploymentType::Bf3),
                &config.dpu_agent_bootstrap_ca,
                &interfaces,
                service_vpc_slots,
                &NodeAuthConfig::default(),
                None,
            );
            carbide_dpf::InitDpfResourcesConfigBuilder::default()
                .deployment_type(DpuDeploymentType::Bf3)
                .deployment_scoped_service_interfaces(true)
                .num_of_vfs(16)
                .services(services.clone())
                .interfaces(interfaces.clone())
                .intercept_bridging(topology.clone())
                .service_vpc_slots(service_vpc_slots)
                .build()
                .expect("production service attachments must match the resolved inventory");
            let hbn = services
                .iter()
                .find(|service| service.name == DOCA_HBN_SERVICE_NAME)
                .expect("mandatory HBN service");
            let dhcp = services
                .iter()
                .find(|service| service.name == DHCP_SERVER_SERVICE_NAME)
                .expect("mandatory DHCP service");
            let fmds = services
                .iter()
                .find(|service| service.name == FMDS_SERVICE_NAME)
                .expect("mandatory FMDS service");

            // HBN's generated interfaces, SF request, and startup YAML must describe the same inventory.
            assert_eq!(hbn.interfaces.len(), 4 + slot_count as usize);
            assert_eq!(
                hbn.helm_values.as_ref().unwrap()["resources"]["nvidia.com/bf_sf"],
                4 + slot_count
            );
            let startup_yaml =
                hbn.config_values.as_ref().unwrap()["configuration"]["startupYAMLJ2"]
                    .as_str()
                    .unwrap();
            assert!(startup_yaml.contains("pf0hpf_if:") && startup_yaml.contains("pf0vf4_if:"));
            assert_eq!(startup_yaml.contains("iface_svc_0:"), slot_count > 0);

            // Render every assembled NAD so the zero-slot case pins exactly the two ordinary attachments.
            let rendered_nads: BTreeMap<_, _> = services
                .iter()
                .flat_map(|service| &service.service_nads)
                .map(|nad| (nad.name.clone(), build_service_nad(nad, TEST_NS, suffix)))
                .collect();
            let mut expected_nads = vec!["mybrsfc-dhcp", "mybrsfc-fmds"];
            if slot_count > 0 {
                expected_nads.push("service-vpc-dhcp-slot0");
            }
            assert_eq!(
                services
                    .iter()
                    .map(|service| service.service_nads.len())
                    .sum::<usize>(),
                expected_nads.len()
            );
            assert_eq!(
                rendered_nads.keys().map(String::as_str).collect::<Vec<_>>(),
                expected_nads
            );
            for (name, nad) in &rendered_nads {
                // Match each NAD to its service contract so the slot listener uses its bridge and
                // jumbo MTU while ordinary DHCP and FMDS remain unchanged.
                let (expected_bridge, expected_mtu) = match name.as_str() {
                    "service-vpc-dhcp-slot0" => ("br-svc-0", SERVICE_VPC_MTU),
                    DHCP_SERVER_SERVICE_NAD_NAME => ("br-sfc", DHCP_SERVER_SERVICE_MTU),
                    FMDS_SERVICE_NAD_NAME => ("br-sfc", FMDS_SERVICE_MTU),
                    _ => unreachable!("rendered NAD names were checked above"),
                };
                assert_eq!(nad.metadata.name.as_deref(), Some(name.as_str()));
                assert_eq!(nad.spec.bridge.as_deref(), Some(expected_bridge));
                assert_eq!(nad.spec.ipam, Some(false));
                assert_eq!(nad.spec.service_mtu, Some(expected_mtu));
                assert!(matches!(
                    nad.spec.resource_type,
                    carbide_dpf::crds::dpuservicenads_generated::DpuServiceNadResourceType::Sf
                ));
            }

            // Render references using production naming; BF3 keeps its ordinary service/NAD names unchanged.
            let nad_rename = rendered_nads
                .iter()
                .map(|(name, nad)| {
                    (
                        name.clone(),
                        nad.metadata.name.clone().expect("rendered NAD name"),
                    )
                })
                .collect();
            let mut expected_dhcp = vec![
                ("d_pf0hpf_if", "mybrsfc-dhcp"),
                ("d_pf0vf4_if", "mybrsfc-dhcp"),
            ];
            if slot_count > 0 {
                expected_dhcp.push(("d_iface_svc_0", "service-vpc-dhcp-slot0"));
            }
            for (service, expected_interfaces) in [
                // DHCP keeps both configured representors and adds only the requested slot listener.
                (dhcp, expected_dhcp),
                // FMDS remains PF-only and keeps its ordinary NAD in either slot mode.
                (fmds, vec![("f_pf0hpf_if", "mybrsfc-fmds")]),
            ] {
                let rendered = build_service_configuration(service, TEST_NS, suffix, &nad_rename);
                assert_eq!(
                    rendered
                        .spec
                        .interfaces
                        .as_ref()
                        .expect("mandatory service interfaces")
                        .iter()
                        .map(|interface| (interface.name.as_str(), interface.network.as_str()))
                        .collect::<Vec<_>>(),
                    expected_interfaces
                );
            }
        }
    }

    /// Verifies operator Helm values cannot disconnect HBN's SF request from its interfaces.
    #[test]
    fn hbn_sf_count_remains_topology_derived() {
        // Attempt to replace the generated SF count while customizing another resource value.
        let mut config = default_doca_hbn_service();
        config.extra_helm_values = serde_json::json!({
            "resources": {
                "memory": "8Gi",
                "nvidia.com/bf_sf": 1,
            }
        })
        .as_object()
        .cloned();
        let interfaces = build_dpu_interfaces_vec();

        // Ordinary resource overrides remain effective, while the SF count follows inventory.
        let hbn = doca_hbn_service(&config, &interfaces, ServiceVpcSlots::default());
        let helm_values = hbn.helm_values.unwrap();
        assert_eq!(helm_values["resources"]["memory"], "8Gi");
        assert_eq!(
            helm_values["resources"]["nvidia.com/bf_sf"],
            interfaces.len()
        );
    }

    #[test]
    fn helm_value_tables_merge_recursively() {
        let mut values = serde_json::json!({
            "image": {
                "repository": "generated",
                "tag": "generated",
            }
        });

        let overlay = serde_json::json!({
            "image": { "tag": "configured" },
            "fmds": { "sign_proxy_url": "http://dsx-imds" },
        });
        merge_helm_values(
            values.as_object_mut().unwrap(),
            overlay.as_object().unwrap(),
        );

        assert_eq!(
            values,
            serde_json::json!({
                "image": {
                    "repository": "generated",
                    "tag": "configured",
                },
                "fmds": { "sign_proxy_url": "http://dsx-imds" },
            })
        );
    }

    #[test]
    fn helm_value_scalars_and_arrays_replace_generated_values() {
        let mut values = serde_json::json!({
            "enabled": false,
            "tolerations": [{ "key": "generated" }],
        });

        let overlay = serde_json::json!({
            "enabled": true,
            "tolerations": [{ "key": "configured" }],
        });
        merge_helm_values(
            values.as_object_mut().unwrap(),
            overlay.as_object().unwrap(),
        );

        assert_eq!(
            values,
            serde_json::json!({
                "enabled": true,
                "tolerations": [{ "key": "configured" }],
            })
        );
    }

    #[test]
    fn dpu_agent_bootstrap_ca_helm_values_follow_site_policy() {
        value_scenarios!(
            run = |policy| {
                dpu_agent_helm_values(
                    &default_dpu_agent_service(),
                    &policy,
                )
                    .get("bootstrapCa")
                    .cloned()
            };
            "legacy download" {
                DpfDpuAgentBootstrapCa::default() => None,
                DpfDpuAgentBootstrapCa::LegacyDownload {
                    url: Some(Url::parse("https://pxe.example.test/site-ca.pem").unwrap()),
                } => Some(serde_json::json!({
                    "source": "legacy_download",
                    "url": "https://pxe.example.test/site-ca.pem",
                })),
            }

            "mounted Kubernetes object" {
                DpfDpuAgentBootstrapCa::Mounted {
                    object_kind: DpfBootstrapCaObjectKind::Secret,
                    name: "nico-bootstrap-ca-v1".to_string(),
                    key: "root.pem".to_string(),
                } => Some(serde_json::json!({
                    "source": "mounted",
                    "object": {
                        "kind": "Secret",
                        "name": "nico-bootstrap-ca-v1",
                        "key": "root.pem",
                    },
                })),
                DpfDpuAgentBootstrapCa::Mounted {
                    object_kind: DpfBootstrapCaObjectKind::ConfigMap,
                    name: "nico-bootstrap-ca-v2".to_string(),
                    key: "ca.crt".to_string(),
                } => Some(serde_json::json!({
                    "source": "mounted",
                    "object": {
                        "kind": "ConfigMap",
                        "name": "nico-bootstrap-ca-v2",
                        "key": "ca.crt",
                    },
                })),
            }
        );
    }

    // ---- imagePullSecrets ----

    #[test]
    fn hbn_and_dts_omit_image_pull_secrets_by_default() {
        // HBN and DTS pull from the public DOCA registry: no imagePullSecrets unless configured.
        let interfaces = build_dpu_interfaces_vec();
        let hbn = doca_hbn_service(
            &default_doca_hbn_service(),
            &interfaces,
            ServiceVpcSlots::default(),
        );
        assert!(
            hbn.helm_values.unwrap().get("imagePullSecrets").is_none(),
            "HBN must not emit imagePullSecrets without a configured secret"
        );

        let dts = dts_service(&default_dts_service());
        assert!(
            dts.helm_values.unwrap().get("imagePullSecrets").is_none(),
            "DTS must not emit imagePullSecrets without a configured secret"
        );
    }

    #[test]
    fn hbn_and_dts_emit_image_pull_secrets_when_configured() {
        let expected = serde_json::json!([{ "name": "private-pull-secret" }]);
        let interfaces = build_dpu_interfaces_vec();

        let mut hbn_cfg = default_doca_hbn_service();
        hbn_cfg.docker_image_pull_secret = Some("private-pull-secret".to_string());
        assert_eq!(
            doca_hbn_service(&hbn_cfg, &interfaces, ServiceVpcSlots::default())
                .helm_values
                .unwrap()["imagePullSecrets"],
            expected
        );

        let mut dts_cfg = default_dts_service();
        dts_cfg.docker_image_pull_secret = Some("private-pull-secret".to_string());
        assert_eq!(
            dts_service(&dts_cfg).helm_values.unwrap()["imagePullSecrets"],
            expected
        );
    }

    #[test]
    fn dts_service_omits_default_image_and_applies_overrides() {
        let default_helm_values = dts_service(&default_dts_service()).helm_values.unwrap();
        assert!(default_helm_values.get("image").is_none());

        let mut config = default_dts_service();
        config.docker_image_tag = "configured-dts-tag".to_string();
        config.docker_repo_url = "registry.example.test/doca/doca_telemetry".to_string();

        let helm_values = dts_service(&config).helm_values.unwrap();
        assert_eq!(helm_values["image"]["tag"], "configured-dts-tag");
        assert_eq!(
            helm_values["image"]["repository"],
            "registry.example.test/doca/doca_telemetry"
        );
    }

    #[test]
    fn deployment_specific_services_emit_image_pull_secrets_when_configured() {
        value_scenarios!(
            run = |service| {
                let (mut config, build): (
                    DpfServiceConfig,
                    fn(&DpfServiceConfig) -> ServiceDefinition,
                ) = match service {
                    DpfExtraService::DocaWeaveDhcpAgent => (
                        default_doca_weave_dhcp_agent_service(),
                        doca_weave_dhcp_agent_service,
                    ),
                    DpfExtraService::DocaWeaveFlowController => (
                        default_doca_weave_flow_controller_service(),
                        |cfg| doca_weave_flow_controller_service(cfg, None),
                    ),
                    DpfExtraService::DocaXplane => (
                        default_doca_xplane_service(),
                        doca_xplane_service,
                    ),
                };
                let rendered_secret = |definition: ServiceDefinition| {
                    definition
                        .helm_values
                        .and_then(|values| {
                            values["imagePullSecrets"][0]["name"]
                                .as_str()
                                .map(str::to_string)
                        })
                };
                let default_secret = rendered_secret(build(&config));
                config.docker_image_pull_secret = Some("private-doca-secret".to_string());
                let configured_secret = rendered_secret(build(&config));
                (default_secret, configured_secret)
            };
            "Weave DHCP agent" {
                DpfExtraService::DocaWeaveDhcpAgent
                    => (None, Some("private-doca-secret".to_string())),
            }

            "Weave flow controller" {
                DpfExtraService::DocaWeaveFlowController
                    => (None, Some("private-doca-secret".to_string())),
            }

            "Xplane" {
                DpfExtraService::DocaXplane
                    => (None, Some("private-doca-secret".to_string())),
            }
        );
    }

    #[test]
    fn carbide_service_image_pull_secrets_are_conditional() {
        // By default a carbide service carries no pull secret and omits the block,
        // rather than rendering the invalid imagePullSecrets: [{ name: null }].
        let dpu_agent = dpu_agent_service(
            &default_dpu_agent_service(),
            &DpfDpuAgentBootstrapCa::default(),
        );
        assert!(
            dpu_agent
                .helm_values
                .unwrap()
                .get("imagePullSecrets")
                .is_none(),
            "a carbide service without a pull secret must omit imagePullSecrets"
        );

        // When a pull secret is configured, the block is emitted with its name.
        let mut cfg = default_dpu_agent_service();
        cfg.docker_image_pull_secret = Some("nico-pull-secret".to_string());
        let agent = dpu_agent_service(&cfg, &DpfDpuAgentBootstrapCa::default());
        assert_eq!(
            agent.helm_values.unwrap()["imagePullSecrets"],
            serde_json::json!([{ "name": "nico-pull-secret" }])
        );
    }

    #[test]
    fn dpu_agent_template_merges_extra_helm_values() {
        let mut config = default_dpu_agent_service();
        config.extra_helm_values = serde_json::json!({
            "fmds": {
                "sign_proxy_url": "http://dsx-imds.dpf-operator-system.svc.cluster.local:8080"
            },
            "image": {
                "tag": "configured-tag"
            }
        })
        .as_object()
        .cloned();
        let service = dpu_agent_service(&config, &DpfDpuAgentBootstrapCa::default());
        let template = build_service_template(&service, TEST_NS, "");
        let values = template.spec.helm_chart.values.unwrap();

        assert_eq!(
            values["fmds"]["sign_proxy_url"],
            "http://dsx-imds.dpf-operator-system.svc.cluster.local:8080"
        );
        assert_eq!(values["image"]["repository"], config.docker_repo_url);
        assert_eq!(values["image"]["tag"], "configured-tag");
    }

    #[test]
    fn xplane_agent_mounts_remain_enabled_when_partial_multiplane_initialization_is_disabled() {
        let mut resolved = DpfResolvedMandatoryServicesConfig {
            base: serde_json::from_value(serde_json::json!({}))
                .expect("mandatory services build from their serde defaults"),
            extra: BTreeMap::from([(DpfExtraService::DocaXplane, default_doca_xplane_service())]),
        };
        // Runtime maintenance requires these mounts even if an overlay disables them.
        resolved.base.dpu_agent.extra_helm_values =
            serde_json::json!({ "xplaneMaintenance": { "enabled": false } })
                .as_object()
                .cloned();
        let ewethers_config = EwEthersConfig {
            enabled: true,
            astra_enabled: true,
            partial_multiplane: crate::cfg::file::PartialMultiplane::Disabled,
            ..Default::default()
        };
        let service = mandatory_services(
            &resolved,
            &DpfDpuAgentBootstrapCa::default(),
            &[],
            ServiceVpcSlots::default(),
            &NodeAuthConfig::default(),
            Some(&ewethers_config),
        )
        .into_iter()
        .find(|service| service.name == DPU_AGENT_SERVICE_NAME)
        .expect("mandatory DPU agent service");
        let template = build_service_template(&service, TEST_NS, "bf4astra");
        let values = template
            .spec
            .helm_chart
            .values
            .expect("helmChart values must be set");

        assert_eq!(values["xplaneMaintenance"]["enabled"], true);
    }

    // ---- weave services ----

    #[test]
    fn weave_dhcp_agent_service_emits_networks_and_sf_resources() {
        let svc = doca_weave_dhcp_agent_service(&default_doca_weave_dhcp_agent_service());
        let helm_values = svc.helm_values.as_ref().expect("helm_values must be set");
        assert_eq!(
            helm_values["weaveDHCPAgent"]["containers"]["weaveDHCPAgent"]["image"]["repository"],
            format!("{DOCA_WEAVE_IMAGE_REGISTRY}/{DOCA_WEAVE_DHCP_AGENT_SERVICE_IMAGE_NAME}")
        );
        assert!(
            helm_values.get("image").is_none(),
            "DHCP-agent image must not use a top-level image key"
        );

        let configuration =
            build_service_configuration(&svc, TEST_NS, "bf4astra", &BTreeMap::new());
        let service_configuration = configuration
            .spec
            .service_configuration
            .expect("serviceConfiguration must be set");
        let values = service_configuration
            .helm_chart
            .expect("helmChart must be set")
            .values
            .expect("helmChart values must be set");

        assert_eq!(values["weaveDHCPAgent"]["enabled"], true);
        assert_eq!(values["weaveDHCPAgent"]["dhcpNetworks"]["createNADs"], true);
        assert_eq!(
            values["weaveDHCPAgent"]["dhcpNetworks"]["networks"],
            serde_json::Value::Array(weave_dhcp_agent_networks())
        );

        let networks = values["weaveDHCPAgent"]["dhcpNetworks"]["networks"]
            .as_array()
            .unwrap();
        assert_eq!(
            networks
                .iter()
                .map(|network| network["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "dhcp-a56", "dhcp-a53", "dhcp-a6", "dhcp-a3", "dhcp-a43", "dhcp-a46", "dhcp-a13",
                "dhcp-a16"
            ]
        );

        let resources = service_configuration
            .service_daemon_set
            .expect("serviceDaemonSet must be set")
            .resources
            .expect("serviceDaemonSet resources must be set");
        assert_eq!(
            resources.get("nvidia.com/bf_sf"),
            Some(&IntOrString::Int(8))
        );
    }

    #[test]
    fn weave_flow_controller_service_emits_underlay_config_values() {
        let svc =
            doca_weave_flow_controller_service(&default_doca_weave_flow_controller_service(), None);
        let helm_values = svc.helm_values.expect("helm_values must be set");
        assert_eq!(
            helm_values["weaveFlowController"]["containers"]["weaveFlowController"]["image"]["repository"],
            format!("{DOCA_WEAVE_IMAGE_REGISTRY}/{DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_IMAGE_NAME}")
        );
        assert!(
            helm_values.get("image").is_none(),
            "flow-controller image must not use a top-level image key"
        );

        let config = svc.config_values.expect("config_values must be set");
        assert_eq!(
            config["weaveFlowController"]["enabled"],
            serde_json::json!(true)
        );
        assert_eq!(
            config["weaveFlowController"]["underlayConfigMapData"]["nicIDType"],
            "mac"
        );
        assert_eq!(
            config["weaveFlowController"]["underlayConfigMapData"]["overlayNetworkPrefixLength"],
            11
        );
        let underlay = &config["weaveFlowController"]["underlayConfigMapData"];
        assert_eq!(underlay["softwarePlaneIDBitLength"], 8);
        assert_eq!(underlay["railIDBitLength"], 4);

        let interfaces = config["weaveFlowController"]["underlayConfigMapData"]["interfaces"]
            .as_array()
            .expect("interfaces must be an array");
        assert_eq!(
            interfaces.len(),
            WEAVE_FLOW_CONTROLLER_UNDERLAY_PORTS.len(),
            "every Astra uplink must be present in underlayConfigMapData.interfaces"
        );
        assert_eq!(
            interfaces.as_slice(),
            weave_flow_controller_underlay_interfaces().as_slice()
        );
    }

    #[test]
    fn weave_flow_controller_service_uses_ewethers_overrides() {
        let resolved = DpfResolvedMandatoryServicesConfig {
            base: serde_json::from_value(serde_json::json!({}))
                .expect("mandatory services build from their serde defaults"),
            extra: BTreeMap::from([(
                DpfExtraService::DocaWeaveFlowController,
                default_doca_weave_flow_controller_service(),
            )]),
        };
        let ewethers_config = EwEthersConfig {
            subnet_mask: 24,
            astra: crate::cfg::file::AstraConfig {
                underlay_ip_rail_id_bit_len: 5,
                underlay_ip_software_plane_id_bit_len: 7,
                ..Default::default()
            },
            ..Default::default()
        };
        let service = mandatory_services(
            &resolved,
            &DpfDpuAgentBootstrapCa::default(),
            &[],
            ServiceVpcSlots::default(),
            &NodeAuthConfig::default(),
            Some(&ewethers_config),
        )
        .into_iter()
        .find(|service| service.name == DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_NAME)
        .expect("Weave flow controller must be present");
        let configuration =
            build_service_configuration(&service, TEST_NS, "bf4astra", &BTreeMap::new());
        let values = configuration
            .spec
            .service_configuration
            .expect("serviceConfiguration must be set")
            .helm_chart
            .expect("helmChart must be set")
            .values
            .expect("helmChart values must be set");
        let underlay = &values["weaveFlowController"]["underlayConfigMapData"];

        assert_eq!(underlay["overlayNetworkPrefixLength"], 24);
        assert_eq!(underlay["softwarePlaneIDBitLength"], 7);
        assert_eq!(underlay["railIDBitLength"], 5);
    }

    // ---- dpu_service_interfaces ----

    #[test]
    fn test_dpu_service_interfaces_hbn_uses_correct_network() {
        let interfaces = build_dpu_interfaces_vec();
        let ifaces =
            dpu_service_interfaces(&interfaces, DOCA_HBN_SERVICE_NAME, DOCA_HBN_SERVICE_NETWORK);
        assert!(!ifaces.is_empty(), "HBN should have at least one interface");
        for iface in &ifaces {
            assert_eq!(
                iface.network, DOCA_HBN_SERVICE_NETWORK,
                "HBN interface '{}' has wrong network",
                iface.name
            );
        }
    }

    #[test]
    fn test_dpu_service_interfaces_dhcp_uses_correct_network() {
        let interfaces = build_dpu_interfaces_vec();
        let ifaces = dpu_service_interfaces(
            &interfaces,
            DHCP_SERVER_SERVICE_NAME,
            DHCP_SERVER_SERVICE_NAD_NAME,
        );
        assert!(
            !ifaces.is_empty(),
            "DHCP server should have at least one interface"
        );
        for iface in &ifaces {
            assert_eq!(
                iface.network, DHCP_SERVER_SERVICE_NAD_NAME,
                "DHCP interface '{}' has wrong network",
                iface.name
            );
        }
    }

    #[test]
    fn test_dpu_service_interfaces_derived_from_build_dpu_interfaces_vec() {
        // Every interface returned for HBN must originate from build_dpu_interfaces_vec.
        let all_ifaces = build_dpu_interfaces_vec();
        let hbn_ifaces =
            dpu_service_interfaces(&all_ifaces, DOCA_HBN_SERVICE_NAME, DOCA_HBN_SERVICE_NETWORK);
        let dhcp_ifaces = dpu_service_interfaces(
            &all_ifaces,
            DHCP_SERVER_SERVICE_NAME,
            DHCP_SERVER_SERVICE_NAD_NAME,
        );

        let all_chained_names: Vec<String> = all_ifaces
            .iter()
            .flat_map(|i| i.chained_svc_if.iter().flatten())
            .map(|(_, ifname)| ifname.clone())
            .collect();

        for iface in hbn_ifaces.iter().chain(dhcp_ifaces.iter()) {
            assert!(
                all_chained_names.contains(&iface.name),
                "Interface '{}' was not derived from build_dpu_interfaces_vec",
                iface.name
            );
        }
    }

    #[test]
    fn test_build_service_interface_physical() {
        let interfaces = build_dpu_interfaces_vec();
        let p0 = interfaces
            .iter()
            .find(|i| i.name == "p0")
            .expect("p0 must exist");
        assert!(matches!(
            &p0.iface_type,
            DpuServiceInterfaceTemplateType::Physical
        ));
        let cr = build_service_interface(p0, TEST_NS);
        assert_eq!(cr.metadata.name.as_deref(), Some("p0"));
        assert_eq!(cr.metadata.namespace.as_deref(), Some(TEST_NS));
        let template_spec = &cr.spec.template.spec.template.spec;
        assert!(
            template_spec.physical.is_some(),
            "physical spec must be set for Physical type"
        );
        assert!(template_spec.pf.is_none());
        assert!(template_spec.vf.is_none());
    }

    #[test]
    fn test_build_service_interface_pf() {
        let interfaces = build_dpu_interfaces_vec();
        let pf0hpf = interfaces
            .iter()
            .find(|i| i.name == "pf0hpf")
            .expect("pf0hpf must exist");
        assert!(matches!(
            &pf0hpf.iface_type,
            DpuServiceInterfaceTemplateType::Pf
        ));
        let cr = build_service_interface(pf0hpf, TEST_NS);
        let template_spec = &cr.spec.template.spec.template.spec;
        assert!(
            template_spec.pf.is_some(),
            "pf spec must be set for Pf type"
        );
        let pf = template_spec
            .pf
            .as_ref()
            .expect("pf spec must remain available for selector validation");
        // The public builder is the legacy unscoped path and must not reconcile a new selector.
        assert!(pf.nic_selector.is_none());
        assert!(template_spec.physical.is_none());
        assert!(template_spec.vf.is_none());
    }

    #[test]
    fn test_build_service_interface_vf() {
        let interfaces = build_dpu_interfaces_vec();
        let pf0vf0 = interfaces
            .iter()
            .find(|i| i.name == "pf0vf0")
            .expect("pf0vf0 must exist");
        assert!(matches!(
            &pf0vf0.iface_type,
            DpuServiceInterfaceTemplateType::Vf
        ));
        let cr = build_service_interface(pf0vf0, TEST_NS);
        let template_spec = &cr.spec.template.spec.template.spec;
        assert!(
            template_spec.vf.is_some(),
            "vf spec must be set for Vf type"
        );
        let vf = template_spec.vf.as_ref().unwrap();
        assert_eq!(vf.pf_id, 0);
        assert_eq!(vf.vf_id, 0);
        assert_eq!(vf.parent_interface_ref.as_deref(), Some("p0"));
        // The public builder is the legacy unscoped path and must not reconcile a new selector.
        assert!(vf.nic_selector.is_none());
        assert!(template_spec.physical.is_none());
        assert!(template_spec.pf.is_none());
    }

    #[test]
    fn test_build_service_interface_label_matches_name() {
        let interfaces = build_dpu_interfaces_vec();
        for iface in &interfaces {
            let cr = build_service_interface(iface, TEST_NS);
            let labels = cr
                .spec
                .template
                .spec
                .template
                .metadata
                .as_ref()
                .and_then(|m| m.labels.as_ref())
                .expect("labels must be present");
            assert_eq!(
                labels.get("interface").map(String::as_str),
                Some(iface.name.as_str()),
                "'interface' label must match iface name for '{}'",
                iface.name
            );
        }
    }

    // ---- xplane manifest generation ----

    /// Exercises `default_doca_xplane_service` end to end: renders the xplane
    /// `ServiceDefinition` into its `DPUServiceTemplate` and
    /// `DPUServiceConfiguration` CRs, serializes both to YAML, and writes the
    /// manifests to disk for inspection. Set `DPF_XPLANE_YAML_OUT_DIR` to choose
    /// the output directory; it defaults to a `dpf-xplane-yaml` subdirectory of
    /// the system temp directory. Run with `--nocapture` to see the paths.
    #[test]
    fn xplane_service_generates_dpu_service_yaml_manifests() {
        use std::fs;
        use std::path::PathBuf;

        use carbide_dpf::build_service_template;

        const SUFFIX: &str = "bf4astra";

        let cfg = default_doca_xplane_service();
        let svc = doca_xplane_service(&cfg);

        let template = build_service_template(&svc, TEST_NS, SUFFIX);
        let configuration = build_service_configuration(&svc, TEST_NS, SUFFIX, &BTreeMap::new());

        let template_yaml =
            serde_yaml::to_string(&template).expect("DPUServiceTemplate must serialize to YAML");
        let configuration_yaml = serde_yaml::to_string(&configuration)
            .expect("DPUServiceConfiguration must serialize to YAML");

        // Round-trip the emitted YAML to prove both manifests are well-formed and
        // that the xplane chart coordinates from `default_doca_xplane_service`
        // survived rendering.
        let template_value: serde_yaml::Value =
            serde_yaml::from_str(&template_yaml).expect("template YAML must be valid");
        assert_eq!(template_value["kind"].as_str(), Some("DPUServiceTemplate"));
        let source = &template_value["spec"]["helmChart"]["source"];
        assert_eq!(source["repoURL"].as_str(), Some(cfg.helm_repo_url.as_str()));
        assert_eq!(source["chart"].as_str(), Some(cfg.helm_chart.as_str()));
        assert_eq!(source["version"].as_str(), Some(cfg.helm_version.as_str()));
        let image = &template_value["spec"]["helmChart"]["values"]["image"];
        assert_eq!(
            image["repository"].as_str(),
            Some(cfg.docker_repo_url.as_str())
        );
        assert_eq!(image["tag"].as_str(), Some(cfg.docker_image_tag.as_str()));

        let configuration_value: serde_yaml::Value =
            serde_yaml::from_str(&configuration_yaml).expect("configuration YAML must be valid");
        assert_eq!(
            configuration_value["kind"].as_str(),
            Some("DPUServiceConfiguration")
        );
        assert_eq!(
            configuration_value["spec"]["deploymentServiceName"].as_str(),
            Some(DOCA_XPLANE_SERVICE_NAME)
        );

        // Persist the manifests so they can be inspected outside the test run.
        let out_dir = std::env::var_os("DPF_XPLANE_YAML_OUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("dpf-xplane-yaml"));
        fs::create_dir_all(&out_dir).expect("output directory must be creatable");

        let template_path = out_dir.join("doca-xplane-dpuservicetemplate.yaml");
        let configuration_path = out_dir.join("doca-xplane-dpuserviceconfiguration.yaml");
        fs::write(&template_path, &template_yaml).expect("template YAML must be writable");
        fs::write(&configuration_path, &configuration_yaml)
            .expect("configuration YAML must be writable");

        assert!(template_path.exists());
        assert!(configuration_path.exists());

        println!("wrote {}", template_path.display());
        println!("wrote {}", configuration_path.display());
    }
    /// The override has to reach the rendered helm values, not just the
    /// resolver — staging a disable is worthless if `mandatory_services` still
    /// reads `enabled` and deploys fmds in token mode anyway.
    #[test]
    fn fmds_helm_values_honor_the_token_mode_override() {
        // Every field carries a serde default, so an empty object yields the
        // stock service set without spelling all six out here.
        let resolved = DpfResolvedMandatoryServicesConfig {
            base: serde_json::from_value(serde_json::json!({}))
                .expect("mandatory services build from their serde defaults"),
            extra: BTreeMap::new(),
        };
        let bootstrap_ca = DpfDpuAgentBootstrapCa::default();

        let fmds_mode = |node_auth: &NodeAuthConfig| {
            mandatory_services(
                &resolved,
                &bootstrap_ca,
                &[],
                ServiceVpcSlots::default(),
                node_auth,
                None,
            )
            .into_iter()
            .find(|s| s.name == FMDS_SERVICE_NAME)
            .and_then(|s| s.helm_values)
            .and_then(|v| v.get("useNodeTokens").and_then(serde_json::Value::as_bool))
            .expect("fmds renders useNodeTokens")
        };

        let derived_on = NodeAuthConfig {
            enabled: true,
            ..NodeAuthConfig::default()
        };
        assert!(fmds_mode(&derived_on), "enabled=true derives token mode");

        let staged_off = NodeAuthConfig {
            enabled: true,
            fmds_use_node_tokens: Some(false),
            ..NodeAuthConfig::default()
        };
        assert!(
            !fmds_mode(&staged_off),
            "the override must win over the derived value"
        );

        assert!(
            !fmds_mode(&NodeAuthConfig::default()),
            "node-auth off still renders cert mode"
        );
    }
    /// Same reasoning for token mode: an overlay setting it true while the API
    /// does not accept bearer tokens would deploy keyless fmds pods whose only
    /// credential is refused, bypassing the startup validation entirely.
    #[test]
    fn an_overlay_cannot_turn_on_fmds_token_mode() {
        let mut cfg = default_fmds_service();
        cfg.extra_helm_values = serde_json::json!({ "useNodeTokens": true })
            .as_object()
            .cloned();

        let service = fmds_service(&cfg, &[], false);
        let values = service.helm_values.expect("fmds renders helm values");

        assert_eq!(
            values.get("useNodeTokens"),
            Some(&serde_json::json!(false)),
            "token mode follows the validated config, not the overlay"
        );
    }
}
