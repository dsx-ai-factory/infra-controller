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
    IntOrString, ServiceDefinition, ServiceInterface, ServiceNAD, ServiceNADResourceType,
    ServiceVpcSlots,
};

use crate::cfg::file::{
    DpfBootstrapCaObjectKind, DpfDpuAgentBootstrapCa, DpfExtraService,
    DpfResolvedMandatoryServicesConfig, DpfServiceConfig, NodeAuthConfig,
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

/// Compile-time helm version (set by CI via VERSION env var). Empty on PR/fork builds.
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

/// Forge DHCP Server service definition.
pub(crate) fn dhcp_server_service(
    cfg: &DpfServiceConfig,
    dpu_interfaces: &[DpuServiceInterfaceTemplateDefinition],
) -> ServiceDefinition {
    let mut helm_values = serde_json::json!({
        "image": {
            "repository": cfg.docker_repo_url,
            "tag": cfg.docker_image_tag,
        }
    });
    apply_helm_values(&mut helm_values, cfg);
    ServiceDefinition {
        helm_values: Some(helm_values),

        interfaces: dhcp_server_service_interfaces(dpu_interfaces),

        service_daemon_set_annotations: Some(BTreeMap::new()),

        service_nad: Some(ServiceNAD {
            name: DHCP_SERVER_SERVICE_NAD_NAME.to_string(),
            bridge: Some("br-sfc".to_string()),
            resource_type: ServiceNADResourceType::Sf,
            ipam: Some(false),
            mtu: Some(DHCP_SERVER_SERVICE_MTU),
        }),

        ..ServiceDefinition::new(
            &cfg.name,
            &cfg.helm_repo_url,
            &cfg.helm_chart,
            &cfg.helm_version,
        )
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

        service_daemon_set_annotations: Some(BTreeMap::new()),

        service_nad: Some(ServiceNAD {
            name: FMDS_SERVICE_NAD_NAME.to_string(),
            bridge: Some("br-sfc".to_string()),
            resource_type: ServiceNADResourceType::Sf,
            ipam: Some(false),
            mtu: Some(FMDS_SERVICE_MTU),
        }),

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

const WEAVE_DHCP_AGENT_NETWORKS: &[&str] = &[
    "r0swpln0", "r1swpln0", "r0swpln1", "r1swpln1", "r2swpln0", "r3swpln0", "r2swpln1", "r3swpln1",
];

fn weave_dhcp_agent_networks() -> Vec<serde_json::Value> {
    WEAVE_DHCP_AGENT_NETWORKS
        .iter()
        .map(|port| {
            serde_json::json!({
                "name": format!("dhcp-{port}"),
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
    ("0005:06:00.0", "r0swpln0"),
    ("0005:03:00.0", "r1swpln0"),
    ("0000:06:00.0", "r2swpln0"),
    ("0000:03:00.0", "r3swpln0"),
    ("0004:03:00.0", "r0swpln1"),
    ("0004:06:00.0", "r1swpln1"),
    ("0001:03:00.0", "r2swpln1"),
    ("0001:06:00.0", "r3swpln1"),
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

pub(crate) fn doca_weave_flow_controller_service(cfg: &DpfServiceConfig) -> ServiceDefinition {
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
                "underlayConfigMapData": {
                    "nicIDType": "mac",
                    "overlayNetworkPrefixLength": 11,
                    "softwarePlaneIDBitLength": 8,
                    "railIDBitLength": 4,
                    "interfaces": weave_flow_controller_underlay_interfaces(),
                }
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
) -> Vec<ServiceDefinition> {
    let mut service_vec = vec![
        dts_service(&resolved.base.dts),
        doca_hbn_service(&resolved.base.doca_hbn, interfaces, service_vpc_slots),
        dhcp_server_service(&resolved.base.dhcp_server, interfaces),
        dpu_agent_service(&resolved.base.dpu_agent, bootstrap_ca),
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
                service_vec.push(doca_weave_flow_controller_service(cfg))
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
        DpfInterceptBridge, DpfInterceptBridging, DpfInterfaceIdentity,
        DpuServiceInterfaceTemplateType,
    };
    use carbide_dpf::{
        build_service_configuration, build_service_interface, build_service_template,
    };
    use carbide_test_support::value_scenarios;
    use url::Url;

    use super::*;

    const TEST_NS: &str = "dpf-operator-system";

    /// Verifies every service definition consumes the same configured effective inventory.
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

        // HBN receives p0, p1, the PF, the VF, and the configured external attachment; its SF
        // count and startup YAML agree.
        let service_vpc_slots = ServiceVpcSlots::new(1).unwrap();
        let hbn = doca_hbn_service(&default_doca_hbn_service(), &interfaces, service_vpc_slots);
        assert_eq!(hbn.interfaces.len(), 5);
        assert_eq!(
            hbn.helm_values.as_ref().unwrap()["resources"]["nvidia.com/bf_sf"],
            5
        );
        let startup_yaml = hbn.config_values.as_ref().unwrap()["configuration"]["startupYAMLJ2"]
            .as_str()
            .unwrap();
        assert!(
            startup_yaml.contains("pf0hpf_if:")
                && startup_yaml.contains("pf0vf4_if:")
                && startup_yaml.contains("iface_svc_0:")
        );

        // DHCP receives both configured entries, while FMDS receives only the PF.
        let dhcp = dhcp_server_service(&default_dhcp_server_service(), &interfaces);
        let fmds = fmds_service(&default_fmds_service(), &interfaces, false);
        assert_eq!(
            dhcp.interfaces
                .iter()
                .map(|interface| interface.name.as_str())
                .collect::<Vec<_>>(),
            ["d_pf0hpf_if", "d_pf0vf4_if"]
        );
        assert_eq!(
            fmds.interfaces
                .iter()
                .map(|interface| interface.name.as_str())
                .collect::<Vec<_>>(),
            ["f_pf0hpf_if"]
        );
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
                        doca_weave_flow_controller_service,
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
        let svc = doca_weave_flow_controller_service(&default_doca_weave_flow_controller_service());
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
        assert_eq!(
            config["weaveFlowController"]["underlayConfigMapData"]["softwarePlaneIDBitLength"],
            8
        );
        assert_eq!(
            config["weaveFlowController"]["underlayConfigMapData"]["railIDBitLength"],
            4
        );

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
