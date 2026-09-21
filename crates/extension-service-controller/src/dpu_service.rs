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

//! Pure projection and ownership validation for NICo-owned `DPUService` CRs.

use std::collections::BTreeMap;

use carbide_dpf::{
    DetachedDpuServiceDefinition, DetachedHelmChart, DetachedServiceDaemonSet,
    DetachedServiceDaemonSetRollingUpdate, DetachedServiceDaemonSetUpdateStrategy,
    DpuServiceObservation, IntOrString,
};
use carbide_uuid::extension_service::ExtensionServiceId;
use model::extension_service::{
    DPF_HELM_CHART_OWNER_LABEL, DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DpfHelmChartIdentity,
    DpfHelmChartIntOrString, DpfHelmChartServiceData,
};
use serde_json::{Map, Value, json};

/// Builds the complete, detached DPUService definition owned by one extension
/// service. The DPF SDK alone converts this definition to the checked CR type.
///
/// The service is deliberately detached until an instance lifecycle operation
/// applies `placement_label_key=enabled` to a DPU.  Its `nodeSelector` is an
/// exact-match selector, so it has no target before that label exists.
pub fn project_dpu_service(
    extension_service_id: ExtensionServiceId,
    namespace: &str,
    data: &DpfHelmChartServiceData,
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
        deploy_in_cluster: false,
        security_privileged: data.security_privileged,
        service_daemon_set: Some(projected_service_daemon_set(&identity, data)),
    }
}

/// Returns a JSON merge patch containing only the mutable DPUService fields
/// that NICo owns.  Identity and attachment-bound fields are intentionally
/// absent: they must be validated before applying this patch, never repaired
/// or overwritten.
///
/// `existing` is used to make tenant-owned objects complete replacements
/// despite JSON Merge Patch's recursive object-merge behavior. Any key absent
/// from a desired object is emitted as `null`, recursively removing it from
/// the live DPUService.
///
/// An absent desired `values` is represented by `null` so the entire stored
/// values object is removed. The full projected CR, by contrast, serializes
/// absent values by omitting that field.
pub fn dpu_service_mutable_patch(
    projected: &DetachedDpuServiceDefinition,
    existing: Option<&DpuServiceObservation>,
) -> Value {
    let helm_chart = &projected.helm_chart;
    let mut helm_chart_patch = Map::from_iter([(
        "source".to_owned(),
        json!({
            "repoURL": helm_chart.repo_url,
            "chart": helm_chart.chart,
            "version": helm_chart.version,
        }),
    )]);
    helm_chart_patch.insert(
        "values".to_owned(),
        helm_chart.values.as_ref().map_or(Value::Null, |values| {
            Value::Object(values_replacement_merge_patch(
                &json_object(values),
                &existing
                    .and_then(|existing| existing.helm_chart.values.as_ref())
                    .map(json_object)
                    .unwrap_or_default(),
            ))
        }),
    );

    // Replace only the tenant-configurable daemon-set fields.
    let service_daemon_set = projected.service_daemon_set.as_ref();
    let existing_service_daemon_set =
        existing.and_then(|existing| existing.service_daemon_set.as_ref());
    let service_daemon_set_patch = Map::from_iter([
        (
            "annotations".to_owned(),
            optional_object_replacement_patch(
                service_daemon_set.and_then(|daemon_set| daemon_set.annotations.as_ref()),
                existing_service_daemon_set.and_then(|daemon_set| daemon_set.annotations.as_ref()),
            ),
        ),
        (
            "labels".to_owned(),
            optional_object_replacement_patch(
                service_daemon_set.and_then(|daemon_set| daemon_set.labels.as_ref()),
                existing_service_daemon_set.and_then(|daemon_set| daemon_set.labels.as_ref()),
            ),
        ),
        (
            "resources".to_owned(),
            optional_object_replacement_patch(
                service_daemon_set.and_then(|daemon_set| daemon_set.resources.as_ref()),
                existing_service_daemon_set.and_then(|daemon_set| daemon_set.resources.as_ref()),
            ),
        ),
        (
            "updateStrategy".to_owned(),
            optional_value_replacement_patch(
                service_daemon_set
                    .and_then(|daemon_set| daemon_set.update_strategy.as_ref())
                    .map(update_strategy_json),
                existing_service_daemon_set
                    .and_then(|daemon_set| daemon_set.update_strategy.clone()),
            ),
        ),
    ]);

    json!({
        "spec": {
            "helmChart": helm_chart_patch,
            "security": {"privileged": projected.security_privileged},
            "serviceDaemonSet": service_daemon_set_patch,
        },
    })
}

/// Converts an optional typed map into a replacement-style JSON Merge Patch so
/// keys removed by a tenant do not survive in the live DPUService.
fn optional_object_replacement_patch<T: serde::Serialize>(
    desired: Option<&BTreeMap<String, T>>,
    existing: Option<&BTreeMap<String, T>>,
) -> Value {
    // Serialize both maps before applying the common JSON replacement logic.
    optional_value_replacement_patch(
        desired.map(|value| serde_json::to_value(value).expect("map serialization is infallible")),
        existing.map(|value| serde_json::to_value(value).expect("map serialization is infallible")),
    )
}

/// Produces a JSON Merge Patch value that replaces objects recursively and
/// clears the live field when the desired value is absent.
fn optional_value_replacement_patch(desired: Option<Value>, existing: Option<Value>) -> Value {
    // Object replacement requires explicit nulls for keys that disappeared;
    // scalar values and absent fields already have direct merge-patch forms.
    match desired {
        Some(Value::Object(desired)) => Value::Object(values_replacement_merge_patch(
            &desired,
            &existing
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default(),
        )),
        Some(value) => value,
        None => Value::Null,
    }
}

fn values_replacement_merge_patch(
    desired: &Map<String, Value>,
    existing: &Map<String, Value>,
) -> Map<String, Value> {
    let mut patch = Map::new();

    for (key, desired_value) in desired {
        let patch_value = match (desired_value, existing.get(key)) {
            (Value::Object(desired), Some(Value::Object(existing))) => {
                Value::Object(values_replacement_merge_patch(desired, existing))
            }
            _ => desired_value.clone(),
        };
        patch.insert(key.clone(), patch_value);
    }

    for key in existing.keys().filter(|key| !desired.contains_key(*key)) {
        patch.insert(key.clone(), Value::Null);
    }

    patch
}

/// Converts the SDK's ordered value map to the representation used inside a
/// `serde_json` document, so the patch builder descends through one map type.
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
    namespace: &str,
) -> Result<(), DpuServiceOwnershipConflict> {
    let identity = DpfHelmChartIdentity::from_service_id(extension_service_id);

    verify_dpu_service_owner_label(existing, extension_service_id)?;

    immutable_field_matches(
        existing.name.as_deref(),
        Some(identity.dpu_service_name.as_str()),
        "metadata.name",
    )?;
    immutable_field_matches(
        existing.namespace.as_deref(),
        Some(namespace),
        "metadata.namespace",
    )?;
    immutable_field_matches(
        existing.deploy_in_cluster,
        Some(false),
        "spec.deployInCluster",
    )?;
    immutable_absent(
        !existing.dpu_cluster_selector_present,
        "spec.dpuClusterSelector",
    )?;
    immutable_absent(existing.service_id.is_none(), "spec.serviceID")?;
    immutable_absent(!existing.interfaces_present, "spec.interfaces")?;
    immutable_absent(!existing.config_ports_present, "spec.configPorts")?;
    immutable_field_matches(
        existing.helm_chart.release_name.as_deref(),
        Some(identity.helm_release_name.as_str()),
        "spec.helmChart.source.releaseName",
    )?;
    let expected_node_selector = node_selector_json(&detached_node_selector_labels(&identity));
    immutable_field_matches(
        existing
            .service_daemon_set
            .as_ref()
            .and_then(|daemon_set| daemon_set.node_selector.as_ref()),
        Some(&expected_node_selector),
        "spec.serviceDaemonSet.nodeSelector",
    )
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
    #[error("DPUService ownership label does not identify this extension service")]
    OwnershipLabel,
    #[error("DPUService immutable field conflicts with NICo contract: {field}")]
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
) -> DetachedServiceDaemonSet {
    let service_daemon_set = data.service_daemon_set.as_ref();

    // Always supply NICo-owned placement while preserving omission of each
    // tenant-configurable field.
    DetachedServiceDaemonSet {
        node_selector_labels: Some(detached_node_selector_labels(identity)),
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

/// Serializes an observed update strategy using the DPF field names needed to
/// calculate a replacement-style merge patch against the desired strategy.
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
            security_privileged: true,
            values,
            service_daemon_set: None,
        }
    }

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
            deploy_in_cluster: Some(projected.deploy_in_cluster),
            dpu_cluster_selector_present: false,
            interfaces_present: false,
            paused: None,
            security_privileged: Some(projected.security_privileged),
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
            service_id: None,
            config_ports_present: false,
        }
    }

    #[test]
    fn projection_builds_the_detached_dpu_service_contract() {
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(Some(Map::from_iter([(
                "image".to_owned(),
                json!({"tag": "1.2.3", "repository": "registry.example.com/tenant/service"}),
            )]))),
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
        assert!(!projected.deploy_in_cluster);
        assert_eq!(
            projected.helm_chart.release_name,
            "extsvc-00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(
            projected.helm_chart.values.as_ref().unwrap()["image"],
            json!({"tag": "1.2.3", "repository": "registry.example.com/tenant/service"})
        );
        assert!(projected.security_privileged);
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

    #[test]
    fn projection_omits_absent_values_and_all_attachment_bound_fields() {
        let projected = project_dpu_service(service_id(), NAMESPACE, &data(None));
        assert!(projected.helm_chart.values.is_none());
        assert!(!projected.deploy_in_cluster);
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
                "security.privileged":true,
                "serviceDaemonSet":{
                    "labels":{"app":"old","remove-me":"value"},
                    "annotations":{"example.com/owner":"old"},
                    "resources":{"nvidia.com/bf_sf":1},
                    "updateStrategy":{"type":"RollingUpdate","rollingUpdate":{"maxSurge":"25%","maxUnavailable":0}}
                }
            }"#,
        )
        .unwrap();
        let initial_projection = project_dpu_service(service_id(), NAMESPACE, &initial);

        // Confirm projection preserves integer quantities and limits.
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

        // Change mutable fields and omit old keys to exercise replacement
        // semantics rather than JSON Merge Patch's default recursive merge.
        let desired = DpfHelmChartServiceData::parse(
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "security.privileged":true,
                "serviceDaemonSet":{
                    "labels":{"app":"new"},
                    "annotations":{},
                    "resources":{"nvidia.com/bf_sf":"2"},
                    "updateStrategy":{"type":"OnDelete"}
                }
            }"#,
        )
        .unwrap();
        let desired_projection = project_dpu_service(service_id(), NAMESPACE, &desired);
        let existing = observation(&initial_projection);
        let patch = dpu_service_mutable_patch(&desired_projection, Some(&existing));

        // Removed nested keys must become null while supplied values replace
        // their observed counterparts.
        assert_eq!(
            patch["spec"]["serviceDaemonSet"],
            json!({
                "annotations": {"example.com/owner": null},
                "labels": {"app": "new", "remove-me": null},
                "resources": {"nvidia.com/bf_sf": "2"},
                "updateStrategy": {
                    "type": "OnDelete",
                    "rollingUpdate": null,
                },
            })
        );

        // Tenant updates must never include NICo-owned placement.
        assert!(
            patch["spec"]["serviceDaemonSet"]
                .get("nodeSelector")
                .is_none()
        );
    }

    #[test]
    fn mutable_patch_has_no_identity_or_attachment_fields() {
        let projected = project_dpu_service(service_id(), NAMESPACE, &data(None));
        let patch = dpu_service_mutable_patch(&projected, None);

        assert_eq!(patch["spec"]["helmChart"]["values"], Value::Null);
        assert!(patch["metadata"].is_null());
        assert!(patch["spec"].get("deployInCluster").is_none());
        assert!(patch["spec"].get("serviceID").is_none());
        assert!(patch["spec"].get("interfaces").is_none());
        assert!(patch["spec"].get("configPorts").is_none());
        assert!(patch["spec"].get("dpuClusterSelector").is_none());
        assert!(
            patch["spec"]["serviceDaemonSet"]
                .get("nodeSelector")
                .is_none()
        );
        assert!(
            patch["spec"]["helmChart"]["source"]
                .get("releaseName")
                .is_none()
        );
    }

    #[test]
    fn mutable_patch_removes_values_omitted_from_the_desired_replacement() {
        let existing_values = BTreeMap::from_iter([
            ("replicas".to_owned(), json!(2)),
            ("debug".to_owned(), json!(true)),
            (
                "image".to_owned(),
                json!({"tag": "1.0.0", "repository": "registry.example.com/old"}),
            ),
        ]);
        let projected = project_dpu_service(
            service_id(),
            NAMESPACE,
            &data(Some(Map::from_iter([
                ("replicas".to_owned(), json!(3)),
                ("image".to_owned(), json!({"tag": "2.0.0"})),
            ]))),
        );

        let mut existing = observation(&projected);
        existing.helm_chart.values = Some(existing_values);
        let patch = dpu_service_mutable_patch(&projected, Some(&existing));

        assert_eq!(
            patch["spec"]["helmChart"]["values"],
            json!({
                "replicas": 3,
                "debug": null,
                "image": {
                    "tag": "2.0.0",
                    "repository": null,
                },
            })
        );
    }

    #[test]
    fn mutable_patch_replaces_an_existing_values_object_with_an_empty_one() {
        let projected = project_dpu_service(service_id(), NAMESPACE, &data(Some(Map::new())));
        let existing_values = BTreeMap::from_iter([("debug".to_owned(), json!(true))]);

        let mut existing = observation(&projected);
        existing.helm_chart.values = Some(existing_values);
        let patch = dpu_service_mutable_patch(&projected, Some(&existing));

        assert_eq!(patch["spec"]["helmChart"]["values"], json!({"debug": null}));
    }

    #[test]
    fn ownership_and_immutable_contract_is_enforced_without_value_diagnostics() {
        let projected = project_dpu_service(service_id(), NAMESPACE, &data(None));
        assert_eq!(
            verify_dpu_service_ownership(&observation(&projected), service_id(), NAMESPACE),
            Ok(())
        );

        let mut wrong_owner = observation(&projected);
        wrong_owner.labels.insert(
            DPF_HELM_CHART_OWNER_LABEL.to_owned(),
            "someone-else".to_owned(),
        );
        assert_eq!(
            verify_dpu_service_ownership(&wrong_owner, service_id(), NAMESPACE),
            Err(DpuServiceOwnershipConflict::OwnershipLabel)
        );

        let mut wrong_release_name = observation(&projected);
        wrong_release_name.helm_chart.release_name = Some("other".to_owned());
        let conflict =
            verify_dpu_service_ownership(&wrong_release_name, service_id(), NAMESPACE).unwrap_err();
        assert_eq!(
            conflict,
            DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.helmChart.source.releaseName",
            }
        );
        assert!(!conflict.to_string().contains("other"));

        let mut attached = wrong_release_name;
        attached.helm_chart.release_name =
            Some("extsvc-00000000-0000-0000-0000-000000000001".to_owned());
        attached.service_id = Some("DPF-assigned-service-id".to_owned());
        assert_eq!(
            verify_dpu_service_ownership(&attached, service_id(), NAMESPACE),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceID",
            })
        );

        let mut wrong_deployment_mode = attached;
        wrong_deployment_mode.service_id = None;
        wrong_deployment_mode.deploy_in_cluster = Some(true);
        assert_eq!(
            verify_dpu_service_ownership(&wrong_deployment_mode, service_id(), NAMESPACE),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.deployInCluster",
            })
        );

        let mut wrong_placement = observation(&projected);
        wrong_placement
            .service_daemon_set
            .as_mut()
            .unwrap()
            .node_selector = Some(json!({
            "nodeSelectorTerms": [{"matchExpressions": []}],
        }));
        assert_eq!(
            verify_dpu_service_ownership(&wrong_placement, service_id(), NAMESPACE),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceDaemonSet.nodeSelector",
            })
        );
    }

    #[test]
    fn delete_ownership_check_requires_only_the_owner_label() {
        let projected = project_dpu_service(service_id(), NAMESPACE, &data(None));
        let mut modified_but_owned = observation(&projected);
        modified_but_owned.deploy_in_cluster = Some(true);

        // NICo must not repair an immutable conflict, but the object is still
        // ours and must remain deletable during extension-service cleanup.
        assert_eq!(
            verify_dpu_service_ownership(&modified_but_owned, service_id(), NAMESPACE),
            Err(DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.deployInCluster",
            })
        );
        assert_eq!(
            verify_dpu_service_owner_label(&modified_but_owned, service_id()),
            Ok(())
        );

        modified_but_owned.labels.clear();
        assert_eq!(
            verify_dpu_service_owner_label(&modified_but_owned, service_id()),
            Err(DpuServiceOwnershipConflict::OwnershipLabel)
        );
    }
}
