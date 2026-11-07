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

//! Builds desired NICo-owned DPF resources and validates live ownership.

use std::collections::BTreeMap;

use carbide_dpf::{
    DetachedDpfResourceObservation, DetachedDpuServiceChainDefinition,
    DetachedDpuServiceDefinition, DetachedDpuServiceInterface,
    DetachedDpuServiceInterfaceDefinition, DetachedDpuServiceSecurity, DetachedHelmChart,
    DetachedServiceDaemonSet, DetachedServiceDaemonSetRollingUpdate,
    DetachedServiceDaemonSetUpdateStrategy, DpuServiceObservation, IntOrString, SERVICE_VPC_MTU,
    ServiceVpcSlots, detached_dpu_service_chain_spec, detached_dpu_service_interface_spec,
};
use carbide_uuid::extension_service::ExtensionServiceId;
use model::extension_service::{
    DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL, DPF_HELM_CHART_OWNER_LABEL,
    DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL,
    DpfHelmChartIdentity, DpfHelmChartIntOrString, DpfHelmChartServiceData,
    DpfHelmChartServiceDataError, ExtensionServiceDeploymentType, ExtensionServiceInterfaceMac,
    ServiceVpcAddressFamily,
};
use serde_json::{Map, Value, json};

const MULTUS_NETWORKS_ANNOTATION: &str = "k8s.v1.cni.cncf.io/networks";
const SERVICE_VPC_INTERFACE_NAME: &str = "svc0";
const SERVICE_VPC_IPV4_NAD: &str = "nico-service-vpc-ipv4";
const SERVICE_VPC_IPV6_NAD: &str = "nico-service-vpc-ipv6";

/// Complete dormant DPF parent inventory for one networked registration.
/// Keeping the related objects together prevents callers from exposing the service before its slot
/// paths exist.
#[derive(Debug, Clone)]
pub struct ServiceVpcRegistrationResources {
    /// Dormant DPUService that consumes the service-side interface.
    pub service: DetachedDpuServiceDefinition,
    /// Service-side and fixed-slot patch interfaces required by the registration.
    pub interfaces: Vec<DetachedDpuServiceInterfaceDefinition>,
    /// Fixed-slot chains connecting each patch interface to the service interface.
    pub chains: Vec<DetachedDpuServiceChainDefinition>,
}

/// Builds the complete, detached DPUService data owned by one extension
/// service based on deployment type. The DPF SDK alone converts this data to
/// the checked CR type.
///
/// Demand-driven services wait until an instance lifecycle operation applies
/// `placement_label_key=enabled` to a DPU. Their `nodeSelector` is an exact-match
/// selector, so they have no target before that label exists. Baseline services
/// omit the selector so DPF can deploy to all managed DPUs without instance attachment.
pub fn project_dpu_service(
    extension_service_id: ExtensionServiceId,
    namespace: &str,
    data: &DpfHelmChartServiceData,
    deployment_type: ExtensionServiceDeploymentType,
) -> DetachedDpuServiceDefinition {
    let identity = DpfHelmChartIdentity::from_service_id(extension_service_id);
    DetachedDpuServiceDefinition {
        name: identity.dpu_service_name.clone(),
        namespace: namespace.to_owned(),
        labels: BTreeMap::from([(
            DPF_HELM_CHART_OWNER_LABEL.to_owned(),
            extension_service_id.to_string(),
        )]),
        helm_chart: projected_helm_chart(&identity, data),
        deploy_in_cluster: data.deploy_in_cluster,
        security: DetachedDpuServiceSecurity {
            privileged: data.security.privileged,
            spiffe: data.security.spiffe.is_some(),
        },
        service_daemon_set: Some(projected_service_daemon_set(
            &identity,
            data,
            deployment_type,
        )),
        service_id: data.service_id.clone(),
        interfaces: None,
    }
}

/// Builds every dormant DPF parent with the chart workload serviceID and persisted MAC so retries
/// and pod recreation retain attachment identity.
pub fn project_service_vpc_registration(
    extension_service_id: ExtensionServiceId,
    namespace: &str,
    data: &DpfHelmChartServiceData,
    family: ServiceVpcAddressFamily,
    mac: ExtensionServiceInterfaceMac,
    slots: ServiceVpcSlots,
) -> Result<ServiceVpcRegistrationResources, DpfHelmChartServiceDataError> {
    // DPF workload identity remains chart-owned even though NICo derives resource names from the
    // registration UUID.
    let workload_service_id = data
        .service_id
        .clone()
        .ok_or(DpfHelmChartServiceDataError::MissingField("serviceID"))?;
    let identity = DpfHelmChartIdentity::from_service_id(extension_service_id);
    let ordinal = mac.interface_ordinal;
    let nad_name = service_vpc_nad_name(family);
    let service_interface_name = identity.service_interface_name(ordinal);
    let fixed_labels = registration_labels(extension_service_id, ordinal, None);
    let generic_selector = detached_node_selector_labels(&identity);
    let mut service_template_labels = fixed_labels.clone();
    service_template_labels.extend(generic_selector.clone());

    // The service-side interface follows the generic service placement label and names the shared
    // raw NAD.
    let service_interface = DetachedDpuServiceInterfaceDefinition {
        name: service_interface_name.clone(),
        namespace: namespace.to_owned(),
        labels: fixed_labels,
        node_selector_labels: generic_selector.clone(),
        template_labels: service_template_labels,
        interface: DetachedDpuServiceInterface::Service {
            service_id: workload_service_id,
            interface_name: SERVICE_VPC_INTERFACE_NAME.to_owned(),
            network: format!("{namespace}/{nad_name}"),
        },
    };

    // Each slot has an independent activation label, patch endpoint, and two-port chain.
    let mut interfaces = Vec::with_capacity(slots.len() as usize + 1);
    let mut chains = Vec::with_capacity(slots.len() as usize);
    interfaces.push(service_interface);
    for (slot, peer_bridge) in slots.bridges() {
        let slot_selector = BTreeMap::from([(
            identity.slot_placement_label_key(ordinal, slot),
            DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned(),
        )]);
        let slot_labels = registration_labels(extension_service_id, ordinal, Some(slot));
        let mut patch_template_labels = slot_labels.clone();
        patch_template_labels.extend(slot_selector.clone());
        let patch_name = identity.slot_patch_interface_name(ordinal, slot);

        interfaces.push(DetachedDpuServiceInterfaceDefinition {
            name: patch_name,
            namespace: namespace.to_owned(),
            labels: slot_labels.clone(),
            node_selector_labels: slot_selector.clone(),
            template_labels: patch_template_labels,
            interface: DetachedDpuServiceInterface::Patch { peer_bridge },
        });
        chains.push(DetachedDpuServiceChainDefinition {
            name: identity.slot_chain_name(ordinal, slot),
            namespace: namespace.to_owned(),
            labels: slot_labels.clone(),
            node_selector_labels: slot_selector.clone(),
            template_labels: slot_labels,
            interface_selectors: vec![generic_selector.clone(), slot_selector],
            service_mtu: SERVICE_VPC_MTU,
        });
    }

    // DPF converts only listed service interfaces to pod attachments, so slot patches stay out of
    // this list.
    let mut service = project_dpu_service(
        extension_service_id,
        namespace,
        data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
    service.interfaces = Some(vec![service_interface_name]);
    let annotations = service
        .service_daemon_set
        .get_or_insert_with(Default::default)
        .annotations
        .get_or_insert_with(BTreeMap::new);
    annotations.insert(
        MULTUS_NETWORKS_ANNOTATION.to_owned(),
        json!([{
            "name": nad_name,
            "namespace": namespace,
            "interface": SERVICE_VPC_INTERFACE_NAME,
            "mac": mac.mac_address.to_string(),
        }])
        .to_string(),
    );

    Ok(ServiceVpcRegistrationResources {
        service,
        interfaces,
        chains,
    })
}

/// Builds an RFC 6902 patch that replaces NICo-owned mutable DPUService fields and submits the
/// observed resourceVersion so Kubernetes rejects stale writes without dropping literal JSON nulls
/// or unowned fields.
pub fn dpu_service_mutable_patch(
    projected: &DetachedDpuServiceDefinition,
    existing: &DpuServiceObservation,
    resource_version: &str,
) -> Value {
    let helm_chart = &projected.helm_chart;
    let mut operations = vec![
        json!({"op": "replace", "path": "/metadata/resourceVersion", "value": resource_version}),
        json!({"op": "add", "path": "/spec/helmChart/source/repoURL", "value": helm_chart.repo_url}),
        json!({"op": "add", "path": "/spec/helmChart/source/chart", "value": helm_chart.chart}),
        json!({"op": "add", "path": "/spec/helmChart/source/version", "value": helm_chart.version}),
    ];
    replace_optional_json_field(
        &mut operations,
        "/spec/helmChart/values",
        helm_chart
            .values
            .as_ref()
            .map(json_object)
            .map(Value::Object),
        existing.helm_chart.values.is_some(),
    );

    // Create `/spec/security` when the observed DPUService lacks it; otherwise update
    // `privileged` and `spiffe` individually so unknown sibling fields survive.
    if let Some(existing_security) = existing.security.as_ref() {
        operations.push(json!({
            "op": "add",
            "path": "/spec/security/privileged",
            "value": projected.security.privileged,
        }));
        replace_optional_json_field(
            &mut operations,
            "/spec/security/spiffe",
            projected.security.spiffe.then(|| json!({})),
            existing_security.spiffe,
        );
    } else {
        let mut security = Map::from_iter([(
            "privileged".to_owned(),
            Value::Bool(projected.security.privileged),
        )]);
        if projected.security.spiffe {
            security.insert("spiffe".to_owned(), json!({}));
        }
        operations.push(json!({
            "op": "add",
            "path": "/spec/security",
            "value": security,
        }));
    }

    // Replace only the tenant-configurable daemon-set fields.
    let service_daemon_set = projected.service_daemon_set.as_ref();
    let existing_service_daemon_set = existing.service_daemon_set.as_ref();
    // Exclude Multus from networked tenant annotation replacement to preserve immutable attachment
    // identity while leaving the same key caller-controlled for unnetworked services.
    let networked = projected.interfaces.is_some();
    let mut desired_annotations = mutable_annotations(
        service_daemon_set.and_then(|daemon_set| daemon_set.annotations.as_ref()),
        networked,
    );
    // The Multus annotation is immutable attachment identity, so carry it through the
    // tenant-owned map replacement instead of allowing the patch to remove it.
    if networked
        && let Some(networks) = existing_service_daemon_set
            .and_then(|daemon_set| daemon_set.annotations.as_ref())
            .and_then(|annotations| annotations.get(MULTUS_NETWORKS_ANNOTATION))
    {
        desired_annotations
            .get_or_insert_default()
            .insert(MULTUS_NETWORKS_ANNOTATION.to_owned(), networks.clone());
    }

    let mutable_fields = [
        (
            "annotations",
            desired_annotations
                .as_ref()
                .map(|value| serde_json::to_value(value).expect("map serialization is infallible")),
            existing_service_daemon_set
                .and_then(|daemon_set| daemon_set.annotations.as_ref())
                .is_some(),
        ),
        (
            "labels",
            service_daemon_set
                .and_then(|daemon_set| daemon_set.labels.as_ref())
                .map(|value| serde_json::to_value(value).expect("map serialization is infallible")),
            existing_service_daemon_set
                .and_then(|daemon_set| daemon_set.labels.as_ref())
                .is_some(),
        ),
        (
            "resources",
            service_daemon_set
                .and_then(|daemon_set| daemon_set.resources.as_ref())
                .map(|value| serde_json::to_value(value).expect("map serialization is infallible")),
            existing_service_daemon_set
                .and_then(|daemon_set| daemon_set.resources.as_ref())
                .is_some(),
        ),
        (
            "updateStrategy",
            service_daemon_set
                .and_then(|daemon_set| daemon_set.update_strategy.as_ref())
                .map(update_strategy_json),
            existing_service_daemon_set
                .and_then(|daemon_set| daemon_set.update_strategy.as_ref())
                .is_some(),
        ),
    ];
    for (field, desired, existing) in mutable_fields {
        replace_optional_json_field(
            &mut operations,
            &format!("/spec/serviceDaemonSet/{field}"),
            desired,
            existing,
        );
    }

    Value::Array(operations)
}

/// Reports whether every NICo-owned mutable DPUService field matches the desired definition,
/// excluding immutable attachment fields checked separately.
pub fn dpu_service_mutable_fields_match(
    projected: &DetachedDpuServiceDefinition,
    existing: &DpuServiceObservation,
) -> bool {
    let desired_daemon_set = projected.service_daemon_set.as_ref();
    let existing_daemon_set = existing.service_daemon_set.as_ref();
    let networked = projected.interfaces.is_some();

    existing.helm_chart.repo_url == projected.helm_chart.repo_url
        && existing.helm_chart.chart.as_deref() == Some(projected.helm_chart.chart.as_str())
        && existing.helm_chart.version == projected.helm_chart.version
        && existing.helm_chart.values == projected.helm_chart.values
        && existing.security.as_ref().is_some_and(|security| {
            security.privileged == Some(projected.security.privileged)
                && security.spiffe == projected.security.spiffe
        })
        && mutable_annotations(
            existing_daemon_set.and_then(|daemon_set| daemon_set.annotations.as_ref()),
            networked,
        ) == mutable_annotations(
            desired_daemon_set.and_then(|daemon_set| daemon_set.annotations.as_ref()),
            networked,
        )
        && existing_daemon_set.and_then(|daemon_set| daemon_set.labels.as_ref())
            == desired_daemon_set.and_then(|daemon_set| daemon_set.labels.as_ref())
        && existing_daemon_set.and_then(|daemon_set| daemon_set.resources.as_ref())
            == desired_daemon_set.and_then(|daemon_set| daemon_set.resources.as_ref())
        && existing_daemon_set.and_then(|daemon_set| daemon_set.update_strategy.as_ref())
            == desired_daemon_set
                .and_then(|daemon_set| daemon_set.update_strategy.as_ref())
                .map(update_strategy_json)
                .as_ref()
}

/// Returns caller-controlled annotations so drift checks and patches compare the same fields
/// without treating the Multus selection as mutable.
fn mutable_annotations(
    annotations: Option<&BTreeMap<String, String>>,
    networked: bool,
) -> Option<BTreeMap<String, String>> {
    annotations.map(|annotations| {
        annotations
            .iter()
            .filter(|(key, _)| !networked || key.as_str() != MULTUS_NETWORKS_ANNOTATION)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    })
}

/// Replaces or removes one optional owned field without using null as a deletion sentinel.
/// RFC 6902 keeps a literal null inside the replacement value distinct from removing the field.
fn replace_optional_json_field(
    operations: &mut Vec<Value>,
    path: &str,
    desired: Option<Value>,
    existing: bool,
) {
    match desired {
        Some(value) => operations.push(json!({"op": "add", "path": path, "value": value})),
        None if existing => operations.push(json!({"op": "remove", "path": path})),
        None => {}
    }
}

/// Converts the SDK's ordered value map to the representation stored in one JSON Patch value.
fn json_object(values: &BTreeMap<String, Value>) -> Map<String, Value> {
    values
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Validates that a live DPUService is the object NICo is allowed to manage.
///
/// Errors name the conflicting contract field only.  They intentionally never
/// include live or desired Helm values, which may contain tenant secrets.
pub fn verify_dpu_service_ownership(
    existing: &DpuServiceObservation,
    extension_service_id: ExtensionServiceId,
    projected: &DetachedDpuServiceDefinition,
) -> Result<(), DpuServiceOwnershipConflict> {
    let identity = DpfHelmChartIdentity::from_service_id(extension_service_id);

    verify_dpu_service_owner_label(existing, extension_service_id)?;

    immutable_field_matches(
        existing.name.as_deref(),
        Some(projected.name.as_str()),
        "metadata.name",
    )?;
    immutable_field_matches(
        existing.namespace.as_deref(),
        Some(projected.namespace.as_str()),
        "metadata.namespace",
    )?;
    immutable_field_matches(
        existing.deploy_in_cluster,
        projected.deploy_in_cluster,
        "spec.deployInCluster",
    )?;
    immutable_absent(
        !existing.dpu_cluster_selector_present,
        "spec.dpuClusterSelector",
    )?;
    immutable_absent(!existing.config_ports_present, "spec.configPorts")?;
    immutable_field_matches(
        existing.helm_chart.release_name.as_deref(),
        Some(identity.helm_release_name.as_str()),
        "spec.helmChart.source.releaseName",
    )?;
    // Compare placement with the desired projection so baseline requires an absent selector.
    let expected_node_selector = projected
        .service_daemon_set
        .as_ref()
        .and_then(|daemon_set| daemon_set.node_selector_labels.as_ref())
        .map(node_selector_json);
    immutable_field_matches(
        existing
            .service_daemon_set
            .as_ref()
            .and_then(|daemon_set| daemon_set.node_selector.as_ref()),
        expected_node_selector.as_ref(),
        "spec.serviceDaemonSet.nodeSelector",
    )?;

    // Validate unrelated immutable fields first so a topology update cannot hide external drift.
    immutable_field_matches(
        existing.service_id.as_deref(),
        projected.service_id.as_deref(),
        "spec.serviceID",
    )?;
    immutable_field_matches(
        existing.interfaces.as_ref(),
        projected.interfaces.as_ref(),
        "spec.interfaces",
    )?;

    // A networked service's explicit MAC selection is attachment identity, not a mutable chart
    // annotation.
    if projected.interfaces.is_some() {
        let expected_networks = projected
            .service_daemon_set
            .as_ref()
            .and_then(|daemon_set| daemon_set.annotations.as_ref())
            .and_then(|annotations| annotations.get(MULTUS_NETWORKS_ANNOTATION));
        immutable_field_matches(
            existing
                .service_daemon_set
                .as_ref()
                .and_then(|daemon_set| daemon_set.annotations.as_ref())
                .and_then(|annotations| annotations.get(MULTUS_NETWORKS_ANNOTATION)),
            expected_networks,
            "spec.serviceDaemonSet.annotations[k8s.v1.cni.cncf.io/networks]",
        )?;
    }

    Ok(())
}

/// Validates the immutable identity of a dynamically managed interface parent.
/// A deterministic name is never sufficient proof of ownership because another actor may already
/// use it.
pub(crate) fn verify_service_interface_identity(
    existing: &DetachedDpfResourceObservation,
    extension_service_id: ExtensionServiceId,
    projected: &DetachedDpuServiceInterfaceDefinition,
) -> Result<(), DpuServiceOwnershipConflict> {
    verify_detached_resource_identity(
        existing,
        extension_service_id,
        &projected.name,
        &projected.namespace,
        &projected.labels,
    )?;

    // Variant-specific attachment fields identify what the parent connects and are never safe to
    // rewrite in place.
    let projected_spec = detached_dpu_service_interface_spec(projected)
        .map_err(|_| DpuServiceOwnershipConflict::ImmutableField { field: "spec" })?;
    for (pointer, field) in [
        (
            "/template/spec/template/spec/interfaceType",
            "spec.interfaceType",
        ),
        (
            "/template/spec/template/spec/service/serviceID",
            "spec.service.serviceID",
        ),
        (
            "/template/spec/template/spec/service/interfaceName",
            "spec.service.interfaceName",
        ),
        (
            "/template/spec/template/spec/service/network",
            "spec.service.network",
        ),
        (
            "/template/spec/template/spec/patch/peerBridge",
            "spec.patch.peerBridge",
        ),
    ] {
        immutable_field_matches(
            existing.spec.pointer(pointer),
            projected_spec.pointer(pointer),
            field,
        )?;
    }
    Ok(())
}

/// Reports whether an owned interface parent has the desired mutable specification.
/// Keeping comparison separate from identity validation permits safe repair without broadening
/// adoption.
pub(crate) fn service_interface_spec_matches(
    existing: &DetachedDpfResourceObservation,
    projected: &DetachedDpuServiceInterfaceDefinition,
) -> Result<bool, DpuServiceOwnershipConflict> {
    detached_dpu_service_interface_spec(projected)
        .map(|spec| existing.spec == spec)
        .map_err(|_| DpuServiceOwnershipConflict::ImmutableField { field: "spec" })
}

/// Validates a dynamically managed interface parent against its identity and desired specification.
pub fn verify_service_interface_ownership(
    existing: &DetachedDpfResourceObservation,
    extension_service_id: ExtensionServiceId,
    projected: &DetachedDpuServiceInterfaceDefinition,
) -> Result<(), DpuServiceOwnershipConflict> {
    verify_service_interface_identity(existing, extension_service_id, projected)?;
    immutable_field_matches(
        service_interface_spec_matches(existing, projected)?,
        true,
        "spec",
    )
}

/// Validates the immutable identity of a dynamically managed chain parent.
/// Labels remain immutable even though NICo may repair selector, port, or MTU drift in the owned
/// spec.
pub(crate) fn verify_service_chain_identity(
    existing: &DetachedDpfResourceObservation,
    extension_service_id: ExtensionServiceId,
    projected: &DetachedDpuServiceChainDefinition,
) -> Result<(), DpuServiceOwnershipConflict> {
    verify_detached_resource_identity(
        existing,
        extension_service_id,
        &projected.name,
        &projected.namespace,
        &projected.labels,
    )
}

/// Reports whether an owned chain parent has the desired mutable specification.
/// Exact comparison ensures a successful repair covers selectors, ports, and MTU together.
pub(crate) fn service_chain_spec_matches(
    existing: &DetachedDpfResourceObservation,
    projected: &DetachedDpuServiceChainDefinition,
) -> Result<bool, DpuServiceOwnershipConflict> {
    detached_dpu_service_chain_spec(projected)
        .map(|spec| existing.spec == spec)
        .map_err(|_| DpuServiceOwnershipConflict::ImmutableField { field: "spec" })
}

/// Validates a dynamically managed chain parent against its identity and desired specification.
pub fn verify_service_chain_ownership(
    existing: &DetachedDpfResourceObservation,
    extension_service_id: ExtensionServiceId,
    projected: &DetachedDpuServiceChainDefinition,
) -> Result<(), DpuServiceOwnershipConflict> {
    verify_service_chain_identity(existing, extension_service_id, projected)?;
    immutable_field_matches(
        service_chain_spec_matches(existing, projected)?,
        true,
        "spec",
    )
}

/// Verifies the shared immutable identity fields of one detached DPF parent.
/// Expected labels are a subset because Kubernetes may add bookkeeping labels outside NICo's
/// contract.
fn verify_detached_resource_identity(
    existing: &DetachedDpfResourceObservation,
    extension_service_id: ExtensionServiceId,
    expected_name: &str,
    expected_namespace: &str,
    expected_labels: &BTreeMap<String, String>,
) -> Result<(), DpuServiceOwnershipConflict> {
    if existing.labels.get(DPF_HELM_CHART_OWNER_LABEL) != Some(&extension_service_id.to_string()) {
        return Err(DpuServiceOwnershipConflict::OwnershipLabel);
    }
    immutable_field_matches(
        existing.name.as_deref(),
        Some(expected_name),
        "metadata.name",
    )?;
    immutable_field_matches(
        existing.namespace.as_deref(),
        Some(expected_namespace),
        "metadata.namespace",
    )?;
    immutable_field_matches(
        expected_labels
            .iter()
            .all(|(key, value)| existing.labels.get(key) == Some(value)),
        true,
        "metadata.labels",
    )
}

/// Builds an RFC 6902 patch that replaces the observed resource version and complete
/// detached-parent specification, rejecting stale repairs and removing stale nested fields.
pub(crate) fn detached_resource_spec_patch(spec: Value, resource_version: &str) -> Value {
    json!([
        {"op": "replace", "path": "/metadata/resourceVersion", "value": resource_version},
        {"op": "replace", "path": "/spec", "value": spec},
    ])
}

/// Verifies the only ownership condition required before deleting a
/// deterministically named DPUService. Immutable fields are intentionally not
/// checked here: an object owned by this service must still be removable when
/// it has been otherwise modified.
pub fn verify_dpu_service_owner_label(
    existing: &DpuServiceObservation,
    extension_service_id: ExtensionServiceId,
) -> Result<(), DpuServiceOwnershipConflict> {
    (existing.labels.get(DPF_HELM_CHART_OWNER_LABEL) == Some(&extension_service_id.to_string()))
        .then_some(())
        .ok_or(DpuServiceOwnershipConflict::OwnershipLabel)
}

/// A non-sensitive reason that NICo must neither patch nor delete a live
/// DPUService.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpuServiceOwnershipConflict {
    #[error("ownership label does not identify this extension service")]
    OwnershipLabel,
    #[error("immutable field does not match this registration: {field}")]
    ImmutableField { field: &'static str },
}

fn projected_helm_chart(
    identity: &DpfHelmChartIdentity,
    data: &DpfHelmChartServiceData,
) -> DetachedHelmChart {
    DetachedHelmChart {
        chart: data.chart_name.clone(),
        release_name: identity.helm_release_name.clone(),
        repo_url: data.repo_url.clone(),
        version: data.chart_version.clone(),
        values: data
            .values
            .as_ref()
            .map(|values| values.clone().into_iter().collect()),
    }
}

/// Projects the extension service's DaemonSet settings and its required
/// NICo-owned placement into the generic detached DPF representation.
fn projected_service_daemon_set(
    identity: &DpfHelmChartIdentity,
    data: &DpfHelmChartServiceData,
    deployment_type: ExtensionServiceDeploymentType,
) -> DetachedServiceDaemonSet {
    let service_daemon_set = data.service_daemon_set.as_ref();

    // Only demand-driven placement is gated on labels from instance attachment.
    DetachedServiceDaemonSet {
        node_selector_labels: match deployment_type {
            ExtensionServiceDeploymentType::DemandDriven => {
                Some(detached_node_selector_labels(identity))
            }
            ExtensionServiceDeploymentType::Baseline => None,
        },
        annotations: service_daemon_set.and_then(|daemon_set| daemon_set.annotations.clone()),
        labels: service_daemon_set.and_then(|daemon_set| daemon_set.labels.clone()),
        resources: service_daemon_set
            .and_then(|daemon_set| daemon_set.resources.as_ref())
            .map(|resources| {
                resources
                    .iter()
                    .map(|(name, quantity)| (name.clone(), projected_int_or_string(quantity)))
                    .collect()
            }),
        update_strategy: service_daemon_set
            .and_then(|daemon_set| daemon_set.update_strategy.as_ref())
            .map(|strategy| DetachedServiceDaemonSetUpdateStrategy {
                strategy_type: strategy.strategy_type.clone(),
                rolling_update: strategy.rolling_update.as_ref().map(|rolling| {
                    DetachedServiceDaemonSetRollingUpdate {
                        max_surge: rolling.max_surge.as_ref().map(projected_int_or_string),
                        max_unavailable: rolling
                            .max_unavailable
                            .as_ref()
                            .map(projected_int_or_string),
                    }
                }),
            }),
    }
}

/// Preserves the integer-or-string representation expected by DPF.
fn projected_int_or_string(value: &DpfHelmChartIntOrString) -> IntOrString {
    match value {
        DpfHelmChartIntOrString::Int(value) => IntOrString::Int(*value),
        DpfHelmChartIntOrString::String(value) => IntOrString::String(value.clone()),
    }
}

/// Serializes an observed update strategy using the DPF field names needed in
/// the RFC 6902 replacement operation for the desired strategy.
fn update_strategy_json(strategy: &DetachedServiceDaemonSetUpdateStrategy) -> Value {
    // Omit absent strategy fields so the replacement helper can distinguish
    // them from explicitly supplied values.
    let mut value = Map::new();
    if let Some(strategy_type) = &strategy.strategy_type {
        value.insert("type".to_owned(), json!(strategy_type));
    }
    if let Some(rolling_update) = &strategy.rolling_update {
        // Preserve the nested rolling-update shape used by the DPF CRD.
        let mut rolling = Map::new();
        if let Some(max_surge) = &rolling_update.max_surge {
            rolling.insert("maxSurge".to_owned(), json!(max_surge));
        }
        if let Some(max_unavailable) = &rolling_update.max_unavailable {
            rolling.insert("maxUnavailable".to_owned(), json!(max_unavailable));
        }
        value.insert("rollingUpdate".to_owned(), Value::Object(rolling));
    }
    Value::Object(value)
}

fn detached_node_selector_labels(identity: &DpfHelmChartIdentity) -> BTreeMap<String, String> {
    BTreeMap::from([(
        identity.placement_label_key.clone(),
        DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned(),
    )])
}

/// Resolves the operator-owned raw NAD for one declared address family.
/// The name is never deployment-suffixed because all registration parents share this site
/// prerequisite.
fn service_vpc_nad_name(family: ServiceVpcAddressFamily) -> &'static str {
    match family {
        ServiceVpcAddressFamily::Ipv4 => SERVICE_VPC_IPV4_NAD,
        ServiceVpcAddressFamily::Ipv6 => SERVICE_VPC_IPV6_NAD,
    }
}

/// Builds the fixed ownership labels propagated through one registration resource.
/// Slot labels are present only on slot-specific patch and chain parents.
fn registration_labels(
    extension_service_id: ExtensionServiceId,
    interface_ordinal: u32,
    slot: Option<u32>,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([
        (
            DPF_HELM_CHART_OWNER_LABEL.to_owned(),
            extension_service_id.to_string(),
        ),
        (
            DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL.to_owned(),
            interface_ordinal.to_string(),
        ),
    ]);
    if let Some(slot) = slot {
        labels.insert(
            DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL.to_owned(),
            slot.to_string(),
        );
    }
    labels
}

fn node_selector_json(labels: &BTreeMap<String, String>) -> Value {
    json!({
        "nodeSelectorTerms": [{
            "matchExpressions": labels.iter().map(|(key, value)| json!({
                "key": key,
                "operator": "In",
                "values": [value],
            })).collect::<Vec<_>>(),
        }],
    })
}

fn immutable_field_matches<T: PartialEq>(
    actual: T,
    expected: T,
    field: &'static str,
) -> Result<(), DpuServiceOwnershipConflict> {
    (actual == expected)
        .then_some(())
        .ok_or(DpuServiceOwnershipConflict::ImmutableField { field })
}

fn immutable_absent(
    is_absent: bool,
    field: &'static str,
) -> Result<(), DpuServiceOwnershipConflict> {
    is_absent
        .then_some(())
        .ok_or(DpuServiceOwnershipConflict::ImmutableField { field })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_dpf::DpuServiceDaemonSetObservation;
    use model::extension_service::{DpfHelmChartServiceDaemonSet, DpfHelmChartServiceSecurity};

    use super::*;

    const SERVICE_ID: &str = "00000000-0000-0000-0000-000000000001";
    const NAMESPACE: &str = "dpf-operator-system";

    fn service_id() -> ExtensionServiceId {
        ExtensionServiceId::from_str(SERVICE_ID).unwrap()
    }

    fn data(values: Option<Map<String, Value>>) -> DpfHelmChartServiceData {
        DpfHelmChartServiceData {
            repo_url: "oci://registry.example.com/charts".to_owned(),
            chart_name: "tenant-service".to_owned(),
            chart_version: "1.2.3".to_owned(),
            deploy_in_cluster: Some(false),
            security: DpfHelmChartServiceSecurity {
                privileged: true,
                spiffe: None,
            },
            values,
            service_daemon_set: None,
            service_id: Some("tenant-service-v1".to_owned()),
        }
    }

    /// Converts a desired DetachedDpuServiceDefinition into a DpuServiceObservation with the
    /// attachment and Kubernetes identity fields required by ownership tests.
    fn observation(projected: &DetachedDpuServiceDefinition) -> DpuServiceObservation {
        let service_daemon_set = projected.service_daemon_set.as_ref();
        DpuServiceObservation {
            name: Some(projected.name.clone()),
            namespace: Some(projected.namespace.clone()),
            labels: projected.labels.clone(),
            is_deleting: false,
            helm_chart: carbide_dpf::DpuServiceHelmChartObservation {
                repo_url: projected.helm_chart.repo_url.clone(),
                chart: Some(projected.helm_chart.chart.clone()),
                version: projected.helm_chart.version.clone(),
                release_name: Some(projected.helm_chart.release_name.clone()),
                values: projected.helm_chart.values.clone(),
            },
            deploy_in_cluster: projected.deploy_in_cluster,
            dpu_cluster_selector_present: false,
            interfaces: projected.interfaces.clone(),
            paused: None,
            security: Some(carbide_dpf::DpuServiceSecurityObservation {
                privileged: Some(projected.security.privileged),
                spiffe: projected.security.spiffe,
            }),
            service_daemon_set: service_daemon_set.map(|daemon_set| {
                DpuServiceDaemonSetObservation {
                    node_selector: daemon_set
                        .node_selector_labels
                        .as_ref()
                        .map(node_selector_json),
                    annotations: daemon_set.annotations.clone(),
                    labels: daemon_set.labels.clone(),
                    resources: daemon_set.resources.clone(),
                    update_strategy: daemon_set
                        .update_strategy
                        .as_ref()
                        .map(update_strategy_json),
                }
            }),
            service_id: projected.service_id.clone(),
            config_ports_present: false,
            uid: Some(format!("uid-{}", projected.name)),
            resource_version: Some("1".to_string()),
        }
    }

    /// Verifies caller identity and DPU-only deployment reach the detached service data.
    #[test]
    fn projection_builds_the_detached_dpu_service_contract() {
        // Supply explicit identity and deployment fields alongside ordinary chart values.
        let mut service_data = data(Some(Map::from_iter([(
            "image".to_owned(),
            json!({"tag": "1.2.3", "repository": "registry.example.com/tenant/service"}),
        )])));
        service_data.service_id = Some("tenant-service-v1".to_owned());
        service_data.deploy_in_cluster = Some(false);
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &service_data,
            ExtensionServiceDeploymentType::DemandDriven,
        );

        assert_eq!(
            projected.name,
            "extsvc-00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(projected.namespace, NAMESPACE);
        assert_eq!(
            projected.labels.get(DPF_HELM_CHART_OWNER_LABEL),
            Some(&SERVICE_ID.to_owned())
        );
        assert_eq!(projected.deploy_in_cluster, Some(false));
        assert_eq!(projected.service_id.as_deref(), Some("tenant-service-v1"));
        assert_eq!(
            projected.helm_chart.release_name,
            "extsvc-00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(
            projected.helm_chart.values.as_ref().unwrap()["image"],
            json!({"tag": "1.2.3", "repository": "registry.example.com/tenant/service"})
        );
        assert!(projected.security.privileged);
        assert!(!projected.security.spiffe);
        assert_eq!(
            projected
                .service_daemon_set
                .as_ref()
                .unwrap()
                .node_selector_labels
                .as_ref()
                .unwrap()
                .clone(),
            BTreeMap::from([(
                "nico/extsvc-00000000-0000-0000-0000-000000000001".to_owned(),
                "enabled".to_owned(),
            )])
        );
    }

    /// Verifies baseline has no node selector, allowing deployment to all DPUs,
    /// and keeps the owner label so NICo can identify it for updates and deletion.
    #[test]
    fn baseline_projection_omits_placement_but_retains_ownership() {
        // Build the DPUService that NICo sends to DPF for a baseline service.
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(None),
            ExtensionServiceDeploymentType::Baseline,
        );

        // Check the owner label still identifies this registration and the
        // node selector is absent so deployment is allowed on all DPUs.
        assert_eq!(
            projected.labels.get(DPF_HELM_CHART_OWNER_LABEL),
            Some(&SERVICE_ID.to_owned())
        );
        assert!(
            projected
                .service_daemon_set
                .as_ref()
                .unwrap()
                .node_selector_labels
                .is_none()
        );
    }

    /// Verifies NICo accepts a baseline DPUService with no node selector and
    /// rejects a present selector, so it cannot adopt a service that conflicts
    /// with the request to deploy to all DPUs.
    #[test]
    fn baseline_ownership_rejects_a_present_selector() {
        // Start from a valid selector-free baseline observation.
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(None),
            ExtensionServiceDeploymentType::Baseline,
        );
        let mut observed = observation(&projected);
        assert_eq!(
            verify_dpu_service_ownership(&observed, service_id(), &projected,),
            Ok(())
        );

        // Reject even an empty selector: baseline requires the field to be absent.
        observed.service_daemon_set.as_mut().unwrap().node_selector = Some(json!({}));
        assert_eq!(
            verify_dpu_service_ownership(&observed, service_id(), &projected,),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceDaemonSet.nodeSelector",
            })
        );
    }

    /// Verifies unnetworked DPUService definitions omit tenant-connectivity fields so existing
    /// unnetworked services retain their established CR representation.
    #[test]
    fn projection_omits_absent_values_and_all_attachment_bound_fields() {
        // Omitted Helm values are independent of omitted service-VPC identity.
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(None),
            ExtensionServiceDeploymentType::DemandDriven,
        );

        // Confirm the unnetworked service retains its deployment mode and absent values or
        // interfaces without gaining a Multus attachment.
        assert!(projected.helm_chart.values.is_none());
        assert_eq!(projected.deploy_in_cluster, Some(false));
        assert!(projected.interfaces.is_none());
        assert!(
            projected
                .service_daemon_set
                .as_ref()
                .and_then(|daemon_set| daemon_set.annotations.as_ref())
                .is_none_or(|annotations| !annotations.contains_key(MULTUS_NETWORKS_ANNOTATION))
        );
    }

    /// Verifies one interface across two slots preserves tenant annotations and produces the exact
    /// chart serviceID, selector, NAD, naming, and MAC contract required by later activation.
    #[test]
    fn service_vpc_projection_builds_one_service_interface_and_two_slot_paths() {
        // Preserve a tenant annotation beside Multus while two validated slots drive only patch
        // and chain fan-out.
        let mut service_data = data(None);
        service_data.service_daemon_set = Some(DpfHelmChartServiceDaemonSet {
            labels: None,
            annotations: Some(BTreeMap::from([(
                "tenant.example.com/preserved".to_owned(),
                "true".to_owned(),
            )])),
            resources: None,
            update_strategy: None,
        });
        let mac = ExtensionServiceInterfaceMac::candidate(service_id(), 0, 0);
        let slots = ServiceVpcSlots::new(2).expect("two slots fit the DPF inventory");
        let projected = project_service_vpc_registration(
            service_id(),
            NAMESPACE,
            &service_data,
            ServiceVpcAddressFamily::Ipv4,
            mac,
            slots,
        )
        .expect("valid service data includes a DPF workload identity");

        // Build the complete expected resource contract without calling the definition builders.
        let owner_labels = BTreeMap::from([
            (DPF_HELM_CHART_OWNER_LABEL.to_owned(), SERVICE_ID.to_owned()),
            (
                DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL.to_owned(),
                "0".to_owned(),
            ),
        ]);
        let generic_selector = BTreeMap::from([(
            format!("nico/extsvc-{SERVICE_ID}"),
            DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned(),
        )]);
        let slot_labels = |slot: u32| {
            BTreeMap::from([
                (DPF_HELM_CHART_OWNER_LABEL.to_owned(), SERVICE_ID.to_owned()),
                (
                    DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL.to_owned(),
                    "0".to_owned(),
                ),
                (
                    DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL.to_owned(),
                    slot.to_string(),
                ),
            ])
        };
        let slot_selector = |slot: u32| {
            BTreeMap::from([(
                format!("nico/extsvc-{SERVICE_ID}-if0-slot{slot}"),
                DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned(),
            )])
        };

        // Networking adds only the service-side pod attachment; it does not replace the chart
        // identity.
        let expected_service_name = format!("extsvc-{SERVICE_ID}");
        let expected_interface_name = format!("{expected_service_name}-if0-svc");
        assert_eq!(
            projected.service.interfaces.as_deref(),
            Some(std::slice::from_ref(&expected_interface_name))
        );
        assert_eq!(
            projected.service.service_id.as_deref(),
            Some("tenant-service-v1")
        );
        let annotations = projected
            .service
            .service_daemon_set
            .as_ref()
            .and_then(|daemon_set| daemon_set.annotations.as_ref())
            .expect("networked services carry daemon-set annotations");
        assert_eq!(
            annotations
                .get("tenant.example.com/preserved")
                .map(String::as_str),
            Some("true")
        );
        let networks = annotations
            .get(MULTUS_NETWORKS_ANNOTATION)
            .expect("networked services carry an explicit Multus selection");
        assert_eq!(
            serde_json::from_str::<Value>(networks).unwrap(),
            json!([{
                "name": "nico-service-vpc-ipv4",
                "namespace": NAMESPACE,
                "interface": SERVICE_VPC_INTERFACE_NAME,
                "mac": mac.mac_address.to_string(),
            }])
        );

        // Exact definitions protect labels, selectors, slot identities, bridge mapping, and chain
        // ports together.
        let mut service_template_labels = owner_labels.clone();
        service_template_labels.extend(generic_selector.clone());
        let mut slot_zero_template_labels = slot_labels(0);
        slot_zero_template_labels.extend(slot_selector(0));
        let mut slot_one_template_labels = slot_labels(1);
        slot_one_template_labels.extend(slot_selector(1));
        assert_eq!(
            projected.interfaces,
            vec![
                DetachedDpuServiceInterfaceDefinition {
                    name: expected_interface_name,
                    namespace: NAMESPACE.to_owned(),
                    labels: owner_labels,
                    node_selector_labels: generic_selector.clone(),
                    template_labels: service_template_labels,
                    interface: DetachedDpuServiceInterface::Service {
                        service_id: "tenant-service-v1".to_owned(),
                        interface_name: SERVICE_VPC_INTERFACE_NAME.to_owned(),
                        network: format!("{NAMESPACE}/nico-service-vpc-ipv4"),
                    },
                },
                DetachedDpuServiceInterfaceDefinition {
                    name: format!("extsvc-{SERVICE_ID}-if0-slot0-patch"),
                    namespace: NAMESPACE.to_owned(),
                    labels: slot_labels(0),
                    node_selector_labels: slot_selector(0),
                    template_labels: slot_zero_template_labels,
                    interface: DetachedDpuServiceInterface::Patch {
                        peer_bridge: "br-svc-0".to_owned(),
                    },
                },
                DetachedDpuServiceInterfaceDefinition {
                    name: format!("extsvc-{SERVICE_ID}-if0-slot1-patch"),
                    namespace: NAMESPACE.to_owned(),
                    labels: slot_labels(1),
                    node_selector_labels: slot_selector(1),
                    template_labels: slot_one_template_labels,
                    interface: DetachedDpuServiceInterface::Patch {
                        peer_bridge: "br-svc-1".to_owned(),
                    },
                },
            ]
        );
        assert_eq!(
            projected.chains,
            vec![
                DetachedDpuServiceChainDefinition {
                    name: format!("extsvc-{SERVICE_ID}-if0-slot0-chain"),
                    namespace: NAMESPACE.to_owned(),
                    labels: slot_labels(0),
                    node_selector_labels: slot_selector(0),
                    template_labels: slot_labels(0),
                    interface_selectors: vec![generic_selector.clone(), slot_selector(0)],
                    service_mtu: SERVICE_VPC_MTU,
                },
                DetachedDpuServiceChainDefinition {
                    name: format!("extsvc-{SERVICE_ID}-if0-slot1-chain"),
                    namespace: NAMESPACE.to_owned(),
                    labels: slot_labels(1),
                    node_selector_labels: slot_selector(1),
                    template_labels: slot_labels(1),
                    interface_selectors: vec![generic_selector, slot_selector(1)],
                    service_mtu: SERVICE_VPC_MTU,
                },
            ]
        );
    }

    /// Rejects service-VPC resource construction without a chart workload identity, preventing
    /// fallback to NICo's UUID-derived resource name.
    #[test]
    fn service_vpc_resources_require_workload_service_id() {
        // Remove the required chart identity from an otherwise valid definition.
        let mut service_data = data(None);
        service_data.service_id = None;

        let result = project_service_vpc_registration(
            service_id(),
            NAMESPACE,
            &service_data,
            ServiceVpcAddressFamily::Ipv4,
            ExtensionServiceInterfaceMac::candidate(service_id(), 0, 0),
            ServiceVpcSlots::new(1).expect("one slot is valid"),
        );

        // Reject construction before any resource can fall back to the UUID-derived DPUService
        // name.
        assert!(matches!(
            result,
            Err(DpfHelmChartServiceDataError::MissingField("serviceID"))
        ));
    }

    /// Verifies networked ownership includes the interface list and MAC-bearing Multus selection.
    /// This prevents an owned same-name DPUService from silently changing attachment identity.
    #[test]
    fn networked_ownership_rejects_attachment_identity_drift() {
        // Build a complete networked definition and its matching live observation.
        let projected = project_service_vpc_registration(
            service_id(),
            NAMESPACE,
            &data(None),
            ServiceVpcAddressFamily::Ipv4,
            ExtensionServiceInterfaceMac::candidate(service_id(), 0, 0),
            ServiceVpcSlots::new(1).expect("one slot is valid"),
        )
        .expect("valid service data includes a DPF workload identity");
        let matching = observation(&projected.service);
        assert_eq!(
            verify_dpu_service_ownership(&matching, service_id(), &projected.service),
            Ok(())
        );

        // Changing the DPF interface list breaks immutable identity independently.
        let mut wrong_interfaces = matching.clone();
        wrong_interfaces.interfaces = Some(vec!["another-interface".to_owned()]);
        assert_eq!(
            verify_dpu_service_ownership(&wrong_interfaces, service_id(), &projected.service,),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.interfaces",
            })
        );
        // Change only the MAC so this case isolates stable attachment identity from the selected
        // network.
        let mut wrong_mac = matching;
        let networks = wrong_mac
            .service_daemon_set
            .as_mut()
            .expect("networked projection includes a daemon set")
            .annotations
            .as_mut()
            .expect("networked projection includes annotations")
            .get_mut(MULTUS_NETWORKS_ANNOTATION)
            .expect("networked projection includes a Multus selection");
        let mut network_selection: Value =
            serde_json::from_str(networks).expect("Multus selection is valid JSON");
        *network_selection
            .pointer_mut("/0/mac")
            .expect("projected Multus selection contains a MAC") = json!("00:11:22:33:44:55");
        *networks = network_selection.to_string();
        assert_eq!(
            verify_dpu_service_ownership(&wrong_mac, service_id(), &projected.service),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceDaemonSet.annotations[k8s.v1.cni.cncf.io/networks]",
            })
        );
    }

    /// Verifies typed daemon-set fields round-trip into a replacement patch
    /// while NICo-owned placement remains excluded from tenant updates.
    #[test]
    fn projection_and_patch_include_typed_daemon_set_fields_but_not_placement() {
        // Build the observed state with both typed scalar variants and keys
        // that the desired replacement will remove.
        let initial = DpfHelmChartServiceData::parse(
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "deployInCluster":false,
                "security":{"privileged":true,"spiffe":{}},
                "serviceDaemonSet":{
                    "labels":{"app":"old","remove-me":"value"},
                    "annotations":{"example.com/owner":"old"},
                    "resources":{"nvidia.com/bf_sf":1},
                    "updateStrategy":{"type":"RollingUpdate","rollingUpdate":{"maxSurge":"25%","maxUnavailable":0}}
                }
            }"#,
        )
        .unwrap();
        let initial_projection = project_dpu_service(
            service_id(),
            NAMESPACE,
            &initial,
            ExtensionServiceDeploymentType::DemandDriven,
        );

        // Confirm the DPUService definition preserves security, integer quantities, and limits.
        assert!(initial_projection.security.spiffe);
        assert_eq!(
            initial_projection
                .service_daemon_set
                .as_ref()
                .unwrap()
                .resources
                .as_ref()
                .unwrap()["nvidia.com/bf_sf"],
            IntOrString::Int(1)
        );
        assert_eq!(
            initial_projection
                .service_daemon_set
                .as_ref()
                .unwrap()
                .update_strategy
                .as_ref()
                .unwrap()
                .rolling_update
                .as_ref()
                .unwrap()
                .max_unavailable,
            Some(IntOrString::Int(0))
        );

        // Change mutable fields and omit old keys to exercise complete replacement semantics.
        let desired = DpfHelmChartServiceData::parse(
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "deployInCluster":false,
                "security":{"privileged":true},
                "serviceDaemonSet":{
                    "labels":{"app":"new"},
                    "annotations":{},
                    "resources":{"nvidia.com/bf_sf":"2"},
                    "updateStrategy":{"type":"OnDelete"}
                }
            }"#,
        )
        .unwrap();
        let desired_projection = project_dpu_service(
            service_id(),
            NAMESPACE,
            &desired,
            ExtensionServiceDeploymentType::DemandDriven,
        );
        let existing = observation(&initial_projection);
        let patch = dpu_service_mutable_patch(&desired_projection, &existing, "1");
        let mut document = json!({
            "metadata": {"resourceVersion": "1"},
            "spec": {
                "helmChart": {
                    "source": {
                        "repoURL": initial_projection.helm_chart.repo_url,
                        "chart": initial_projection.helm_chart.chart,
                        "version": initial_projection.helm_chart.version,
                    },
                },
                "security": {"privileged": true, "spiffe": {}},
                "serviceDaemonSet": {
                    "annotations": {"example.com/owner": "old"},
                    "labels": {"app": "old", "remove-me": "value"},
                    "nodeSelector": {"matchLabels": {"placement": "preserved"}},
                    "resources": {"nvidia.com/bf_sf": 1},
                    "updateStrategy": {
                        "type": "RollingUpdate",
                        "rollingUpdate": {"maxSurge": "25%", "maxUnavailable": 0},
                    },
                },
            },
        });

        // Applying the patch must replace complete tenant maps without touching placement.
        let patch = serde_json::from_value::<json_patch::Patch>(patch).unwrap();
        json_patch::patch(&mut document, &patch).unwrap();
        assert_eq!(
            document["spec"]["serviceDaemonSet"],
            json!({
                "annotations": {},
                "labels": {"app": "new"},
                "nodeSelector": {"matchLabels": {"placement": "preserved"}},
                "resources": {"nvidia.com/bf_sf": "2"},
                "updateStrategy": {"type": "OnDelete"},
            })
        );
        assert!(document["spec"]["security"].get("spiffe").is_none());
    }

    /// Verifies updates omit immutable identity and filter the Multus annotation from both sides.
    /// Existing attachment identity must survive while unrelated stale annotations remain
    /// replaceable.
    #[test]
    fn mutable_patch_omits_immutable_identity_and_attachment_fields() {
        // Build a valid networked definition so the test changes only mutable attachment fields.
        let projected = project_service_vpc_registration(
            service_id(),
            NAMESPACE,
            &data(None),
            ServiceVpcAddressFamily::Ipv4,
            ExtensionServiceInterfaceMac::candidate(service_id(), 0, 0),
            ServiceVpcSlots::new(1).unwrap(),
        )
        .expect("valid service data includes a DPF workload identity")
        .service;
        let mut existing = observation(&projected);
        // Add one stale tenant annotation beside Multus so the patch must delete only the mutable
        // key.
        existing
            .service_daemon_set
            .as_mut()
            .expect("networked projection includes a daemon set")
            .annotations
            .as_mut()
            .expect("networked projection includes annotations")
            .insert("example.com/stale".to_owned(), "remove-me".to_owned());
        let patch = dpu_service_mutable_patch(&projected, &existing, "1");

        // Restrict every operation to mutable fields while preserving the Multus selection and
        // removing stale tenant annotations.
        assert_eq!(
            patch[0],
            json!({
                "op": "replace",
                "path": "/metadata/resourceVersion",
                "value": "1",
            })
        );
        let paths = patch
            .as_array()
            .expect("JSON Patch is an operation array")
            .iter()
            .filter_map(|operation| operation["path"].as_str())
            .collect::<Vec<_>>();
        assert!(!paths.iter().any(|path| {
            path.contains("serviceID")
                || path.contains("deployInCluster")
                || path.contains("interfaces")
                || path.contains("configPorts")
                || path.contains("dpuClusterSelector")
                || path.contains("nodeSelector")
                || path.contains("releaseName")
        }));
        let annotation_operation = patch
            .as_array()
            .unwrap()
            .iter()
            .find(|operation| operation["path"] == "/spec/serviceDaemonSet/annotations")
            .expect("annotations are replaced");
        let expected_annotations = BTreeMap::from([(
            MULTUS_NETWORKS_ANNOTATION.to_owned(),
            existing
                .service_daemon_set
                .as_ref()
                .unwrap()
                .annotations
                .as_ref()
                .unwrap()[MULTUS_NETWORKS_ANNOTATION]
                .clone(),
        )]);
        assert_eq!(
            annotation_operation["value"],
            serde_json::to_value(expected_annotations).unwrap()
        );
    }

    /// Verifies RFC 6902 replacement preserves literal nulls and unrelated fields when
    /// `/spec/serviceDaemonSet` is absent. A merge patch would delete the null-valued keys and
    /// cause every later Ready scan to detect drift, while nested DaemonSet operations would reject
    /// platform services that omit `/spec/serviceDaemonSet`.
    #[test]
    fn mutable_patch_preserves_literal_nulls_and_unowned_fields() {
        // Define stale live values to exercise replacement of both scalar and nested Helm data.
        let existing_values = BTreeMap::from_iter([
            ("replicas".to_owned(), json!(2)),
            ("debug".to_owned(), json!(true)),
            (
                "image".to_owned(),
                json!({"tag": "1.0.0", "repository": "registry.example.com/old"}),
            ),
        ]);
        // Build desired Helm values with literal nulls to exercise their distinction from removal.
        let mut projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(Some(Map::from_iter([
                ("replicas".to_owned(), json!(3)),
                ("featureOverride".to_owned(), Value::Null),
                (
                    "image".to_owned(),
                    json!({"tag": "2.0.0", "pullPolicy": null}),
                ),
            ]))),
            ExtensionServiceDeploymentType::DemandDriven,
        );

        // Match the platform release by omitting workload settings from desired and observed state.
        projected.service_daemon_set = None;
        let mut existing = observation(&projected);
        // Replace the projected values with stale live data so the patch must preserve nested
        // nulls.
        existing.helm_chart.values = Some(existing_values);
        let patch = dpu_service_mutable_patch(&projected, &existing, "17");
        // Model a raw Kubernetes document with unrelated fields and no `/spec/serviceDaemonSet`
        // object.
        let mut document = json!({
            "metadata": {
                "resourceVersion": "17",
                "annotations": {"external.example.com/owner": "preserved"},
            },
            "spec": {
                "externalField": {"preserved": true},
                "helmChart": {
                    "source": {
                        "repoURL": projected.helm_chart.repo_url,
                        "chart": projected.helm_chart.chart,
                        "version": projected.helm_chart.version,
                        "releaseName": projected.helm_chart.release_name,
                    },
                    "values": existing.helm_chart.values,
                },
                "security": {
                    "privileged": false,
                    "externalPolicy": {"mode": "enforced"},
                },
            },
        });

        // Apply and reread the complete document so transport semantics, not operation shape, prove
        // the contract.
        let patch = serde_json::from_value::<json_patch::Patch>(patch).unwrap();
        json_patch::patch(&mut document, &patch).unwrap();
        assert_eq!(
            document["spec"]["helmChart"]["values"],
            json!({
                "replicas": 3,
                "featureOverride": null,
                "image": {"tag": "2.0.0", "pullPolicy": null},
            })
        );
        assert_eq!(
            document["metadata"]["annotations"]["external.example.com/owner"],
            "preserved"
        );
        assert_eq!(
            document["spec"]["externalField"],
            json!({"preserved": true})
        );
        assert_eq!(
            document["spec"]["helmChart"]["source"]["releaseName"],
            projected.helm_chart.release_name
        );
        assert_eq!(
            document["spec"]["security"]["externalPolicy"],
            json!({"mode": "enforced"})
        );
        assert!(document.pointer("/spec/serviceDaemonSet").is_none());

        // Model the next Kubernetes observation from the applied values; Ready must now stay quiet.
        existing.helm_chart.values =
            Some(serde_json::from_value(document["spec"]["helmChart"]["values"].clone()).unwrap());
        assert!(dpu_service_mutable_fields_match(&projected, &existing));
    }

    /// Verifies mutable patches distinguish an explicitly empty Helm values object from an absent
    /// one because Helm assigns them different chart semantics.
    #[test]
    fn mutable_patch_distinguishes_empty_and_absent_values() {
        let cases = [
            // Preserve an explicit empty object so Helm receives an intentional values override.
            ("explicit empty values", Some(Map::new()), Some(json!({}))),
            // Remove stale values when the caller omits the field entirely.
            ("absent values", None, None),
        ];

        for (scenario, desired_values, expected_values) in cases {
            let projected = project_dpu_service(
                service_id(),
                NAMESPACE,
                &data(desired_values),
                ExtensionServiceDeploymentType::DemandDriven,
            );
            let mut existing = observation(&projected);
            // Seed one stale entry so the desired presence controls replacement versus removal.
            existing.helm_chart.values =
                Some(BTreeMap::from_iter([("debug".to_owned(), json!(true))]));
            let patch = dpu_service_mutable_patch(&projected, &existing, "1");
            let mut document = json!({
                "metadata": {"resourceVersion": "1"},
                "spec": {
                    "helmChart": {
                        "source": {
                            "repoURL": projected.helm_chart.repo_url,
                            "chart": projected.helm_chart.chart,
                            "version": projected.helm_chart.version,
                        },
                        "values": existing.helm_chart.values,
                    },
                    "security": {"privileged": false},
                    "serviceDaemonSet": {"nodeSelector": {}},
                },
            });

            // Apply every operation so the final document proves the requested presence semantics.
            let patch = serde_json::from_value::<json_patch::Patch>(patch).unwrap();
            json_patch::patch(&mut document, &patch).unwrap();
            assert_eq!(
                document.pointer("/spec/helmChart/values"),
                expected_values.as_ref(),
                "{scenario}"
            );
        }
    }

    /// Verifies mutable convergence covers chart, security, and daemon-set fields while excluding
    /// Multus identity.
    /// Proves Ready reconciliation repairs tenant drift without patching attachment identity.
    #[test]
    fn mutable_field_matching_uses_the_patch_ownership_boundary() {
        // Start with a faithful networked observation so every owned mutable field is converged.
        let projected = project_service_vpc_registration(
            service_id(),
            NAMESPACE,
            &data(None),
            ServiceVpcAddressFamily::Ipv4,
            ExtensionServiceInterfaceMac::candidate(service_id(), 0, 0),
            ServiceVpcSlots::new(1).expect("one slot is valid"),
        )
        .expect("valid service data includes a DPF workload identity")
        .service;
        let matching = observation(&projected);
        assert!(dpu_service_mutable_fields_match(&projected, &matching));

        // Chart drift must trigger repair because the patch owns the chart source and values.
        let mut chart_drift = matching.clone();
        chart_drift.helm_chart.version = "9.9.9".to_owned();
        assert!(!dpu_service_mutable_fields_match(&projected, &chart_drift));

        // Security drift is independently mutable even when immutable service identity still
        // matches.
        let mut security_drift = matching.clone();
        security_drift
            .security
            .as_mut()
            .expect("the projection supplies security")
            .privileged = Some(false);
        assert!(!dpu_service_mutable_fields_match(
            &projected,
            &security_drift
        ));

        // Caller-controlled daemon-set labels are replacement fields and cannot silently drift.
        let mut daemon_drift = matching.clone();
        daemon_drift
            .service_daemon_set
            .as_mut()
            .expect("the projection supplies daemon-set settings")
            .labels = Some(BTreeMap::from([("app".to_owned(), "other".to_owned())]));
        assert!(!dpu_service_mutable_fields_match(&projected, &daemon_drift));

        // Resource drift must trigger repair so the workload keeps its caller-supplied limits.
        let mut resource_drift = matching.clone();
        resource_drift
            .service_daemon_set
            .as_mut()
            .expect("the projection supplies daemon-set settings")
            .resources = Some(BTreeMap::from([(
            "cpu".to_owned(),
            IntOrString::String("2".to_owned()),
        )]));
        assert!(!dpu_service_mutable_fields_match(
            &projected,
            &resource_drift
        ));

        // Update-strategy drift must trigger repair because it changes workload rollout behavior.
        let mut strategy_drift = matching.clone();
        strategy_drift
            .service_daemon_set
            .as_mut()
            .expect("the projection supplies daemon-set settings")
            .update_strategy = Some(json!({"type": "OnDelete"}));
        assert!(!dpu_service_mutable_fields_match(
            &projected,
            &strategy_drift
        ));

        // Multus is immutable attachment identity, so mutable matching deliberately ignores it.
        let mut multus_drift = matching;
        *multus_drift
            .service_daemon_set
            .as_mut()
            .expect("networked projection includes a daemon set")
            .annotations
            .as_mut()
            .expect("networked projection includes annotations")
            .get_mut(MULTUS_NETWORKS_ANNOTATION)
            .expect("networked projection includes a Multus selection") = "different".to_owned();
        assert!(dpu_service_mutable_fields_match(&projected, &multus_drift));
        assert_eq!(
            verify_dpu_service_ownership(&multus_drift, service_id(), &projected),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceDaemonSet.annotations[k8s.v1.cni.cncf.io/networks]",
            })
        );
    }

    /// Verifies foreign or rewired DPF parents fail identity checks while selector and chain-spec
    /// drift remains repairable, preventing adoption of foreign topology without rejecting owned
    /// mutable drift.
    #[test]
    fn detached_parent_validation_separates_attachment_identity_from_mutable_spec() {
        // Build one service interface, one patch interface, and one chain with matching Kubernetes
        // observations.
        let projected = project_service_vpc_registration(
            service_id(),
            NAMESPACE,
            &data(None),
            ServiceVpcAddressFamily::Ipv4,
            ExtensionServiceInterfaceMac::candidate(service_id(), 0, 0),
            ServiceVpcSlots::new(1).expect("one slot is valid"),
        )
        .expect("valid service data includes a DPF workload identity");
        let interface = &projected.interfaces[0];
        let patch_interface = &projected.interfaces[1];
        let chain = &projected.chains[0];
        let matching_interface = DetachedDpfResourceObservation {
            name: Some(interface.name.clone()),
            namespace: Some(interface.namespace.clone()),
            labels: interface.labels.clone(),
            spec: detached_dpu_service_interface_spec(interface).unwrap(),
            uid: Some("interface-uid".to_owned()),
            resource_version: Some("1".to_owned()),
            is_deleting: false,
        };
        let matching_patch_interface = DetachedDpfResourceObservation {
            name: Some(patch_interface.name.clone()),
            namespace: Some(patch_interface.namespace.clone()),
            labels: patch_interface.labels.clone(),
            spec: detached_dpu_service_interface_spec(patch_interface).unwrap(),
            uid: Some("patch-interface-uid".to_owned()),
            resource_version: Some("1".to_owned()),
            is_deleting: false,
        };
        let matching_chain = DetachedDpfResourceObservation {
            name: Some(chain.name.clone()),
            namespace: Some(chain.namespace.clone()),
            labels: chain.labels.clone(),
            spec: detached_dpu_service_chain_spec(chain).unwrap(),
            uid: Some("chain-uid".to_owned()),
            resource_version: Some("1".to_owned()),
            is_deleting: false,
        };
        assert_eq!(
            verify_service_interface_ownership(&matching_interface, service_id(), interface),
            Ok(())
        );
        assert_eq!(
            verify_service_chain_ownership(&matching_chain, service_id(), chain),
            Ok(())
        );

        // Reject a matching name and spec when the owner label belongs to another extension
        // service.
        let mut foreign_interface = matching_interface.clone();
        foreign_interface.labels.insert(
            DPF_HELM_CHART_OWNER_LABEL.to_owned(),
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000002")
                .unwrap()
                .to_string(),
        );
        assert_eq!(
            verify_service_interface_ownership(&foreign_interface, service_id(), interface),
            Err(DpuServiceOwnershipConflict::OwnershipLabel)
        );

        let mut wrong_interface_type = matching_interface.clone();
        wrong_interface_type.spec["template"]["spec"]["template"]["spec"]["interfaceType"] =
            json!("patch");
        let mut wrong_service_id = matching_interface.clone();
        wrong_service_id.spec["template"]["spec"]["template"]["spec"]["service"]["serviceID"] =
            json!("another-service");
        let mut wrong_interface_name = matching_interface.clone();
        wrong_interface_name.spec["template"]["spec"]["template"]["spec"]["service"]["interfaceName"] =
            json!("another0");
        let mut wrong_network = matching_interface.clone();
        wrong_network.spec["template"]["spec"]["template"]["spec"]["service"]["network"] =
            json!(format!("{NAMESPACE}/{SERVICE_VPC_IPV6_NAD}"));
        let mut wrong_peer_bridge = matching_patch_interface;
        wrong_peer_bridge.spec["template"]["spec"]["template"]["spec"]["patch"]["peerBridge"] =
            json!("another-bridge");

        for (name, observed, desired, expected_field) in [
            // Changing the interface variant can connect a service endpoint as an unrelated patch
            // interface.
            (
                "interface type",
                wrong_interface_type,
                interface,
                "spec.interfaceType",
            ),
            // Changing the workload identity can attach the parent to another DPF service.
            (
                "service ID",
                wrong_service_id,
                interface,
                "spec.service.serviceID",
            ),
            // Changing the in-pod name can redirect traffic away from NICo's Multus attachment.
            (
                "interface name",
                wrong_interface_name,
                interface,
                "spec.service.interfaceName",
            ),
            // Changing the NAD can expose the workload through a different network.
            ("network", wrong_network, interface, "spec.service.network"),
            // Changing the peer bridge can connect a slot parent to a different datapath.
            (
                "peer bridge",
                wrong_peer_bridge,
                patch_interface,
                "spec.patch.peerBridge",
            ),
        ] {
            assert_eq!(
                verify_service_interface_identity(&observed, service_id(), desired),
                Err(DpuServiceOwnershipConflict::ImmutableField {
                    field: expected_field,
                }),
                "{name}",
            );
        }

        // Allow node-selector drift to reach whole-spec repair without changing what the interface
        // attaches.
        let mut mutable_interface_drift = matching_interface;
        mutable_interface_drift.spec["template"]["spec"]["nodeSelector"]["matchLabels"] =
            json!({"example.com/alternate-placement": "enabled"});
        assert_eq!(
            verify_service_interface_identity(&mutable_interface_drift, service_id(), interface),
            Ok(())
        );
        assert_eq!(
            service_interface_spec_matches(&mutable_interface_drift, interface),
            Ok(false)
        );

        // Allow chain placement and MTU drift to reach whole-spec repair because the selected
        // interfaces remain unchanged.
        let mut alternate_chain = chain.clone();
        alternate_chain.node_selector_labels.insert(
            "example.com/alternate-slot".to_owned(),
            "enabled".to_owned(),
        );
        alternate_chain.service_mtu += 1;
        let mut drifted_chain = matching_chain;
        drifted_chain.spec = detached_dpu_service_chain_spec(&alternate_chain).unwrap();
        assert_eq!(
            verify_service_chain_identity(&drifted_chain, service_id(), chain),
            Ok(())
        );
        assert_eq!(service_chain_spec_matches(&drifted_chain, chain), Ok(false));
        assert_eq!(
            verify_service_chain_ownership(&drifted_chain, service_id(), chain),
            Err(DpuServiceOwnershipConflict::ImmutableField { field: "spec" })
        );
    }

    /// Verifies unnetworked ownership rejects label, release, deployment, and placement drift.
    /// Diagnostics must identify only contract fields so tenant Helm values never enter failures.
    #[test]
    fn ownership_and_immutable_contract_is_enforced_without_value_diagnostics() {
        // Include a sensitive sentinel so ownership failures must not serialize tenant values.
        let sensitive_value = "registry-password-that-must-not-appear";
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(Some(Map::from_iter([(
                "registryPassword".to_owned(),
                json!(sensitive_value),
            )]))),
            ExtensionServiceDeploymentType::DemandDriven,
        );

        // Confirm the unchanged DPUService satisfies the complete unnetworked contract.
        assert_eq!(
            verify_dpu_service_ownership(&observation(&projected), service_id(), &projected,),
            Ok(())
        );

        // A same-name object from another owner is never adopted.
        let mut wrong_owner = observation(&projected);
        wrong_owner.labels.insert(
            DPF_HELM_CHART_OWNER_LABEL.to_owned(),
            "someone-else".to_owned(),
        );
        assert_eq!(
            verify_dpu_service_ownership(&wrong_owner, service_id(), &projected,),
            Err(DpuServiceOwnershipConflict::OwnershipLabel)
        );

        // Helm release identity stays UUID-derived and its live value is absent from diagnostics.
        let mut wrong_release_name = observation(&projected);
        wrong_release_name.helm_chart.release_name = Some("other".to_owned());
        let conflict = verify_dpu_service_ownership(&wrong_release_name, service_id(), &projected)
            .unwrap_err();
        assert_eq!(
            conflict,
            DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.helmChart.source.releaseName",
            }
        );
        assert!(!conflict.to_string().contains("other"));
        assert!(!conflict.to_string().contains(sensitive_value));

        // Reject host-cluster deployment to preserve DPU-only isolation.
        let mut wrong_deployment_mode = observation(&projected);
        wrong_deployment_mode.deploy_in_cluster = Some(true);
        assert_eq!(
            verify_dpu_service_ownership(&wrong_deployment_mode, service_id(), &projected,),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.deployInCluster",
            })
        );

        // Placement drift could expose a dormant service to unintended DPU nodes.
        let mut wrong_placement = observation(&projected);
        wrong_placement
            .service_daemon_set
            .as_mut()
            .unwrap()
            .node_selector = Some(json!({
            "nodeSelectorTerms": [{"matchExpressions": []}],
        }));
        assert_eq!(
            verify_dpu_service_ownership(&wrong_placement, service_id(), &projected,),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceDaemonSet.nodeSelector",
            })
        );
    }

    /// Verifies deletion trusts the owner label even when another immutable field was altered.
    /// Otherwise an owned but drifted resource could block global service cleanup permanently.
    #[test]
    fn delete_ownership_check_requires_only_the_owner_label() {
        // Introduce immutable drift while retaining the one label that grants cleanup authority.
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(None),
            ExtensionServiceDeploymentType::DemandDriven,
        );
        let mut modified_but_owned = observation(&projected);
        modified_but_owned.deploy_in_cluster = Some(true);

        // NICo must not repair an immutable conflict, but the object is still
        // ours and must remain deletable during extension-service cleanup.
        assert_eq!(
            verify_dpu_service_ownership(&modified_but_owned, service_id(), &projected,),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.deployInCluster",
            })
        );
        assert_eq!(
            verify_dpu_service_owner_label(&modified_but_owned, service_id()),
            Ok(())
        );

        // Remove the owner label to prove matching names cannot grant deletion authority.
        modified_but_owned.labels.clear();
        assert_eq!(
            verify_dpu_service_owner_label(&modified_but_owned, service_id()),
            Err(DpuServiceOwnershipConflict::OwnershipLabel)
        );
    }
}
