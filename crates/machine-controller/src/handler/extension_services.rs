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

//! Instance extension-service reconciliation and status handling.

use std::collections::{BTreeMap, HashMap, HashSet};

use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::machine::DpuMachineId;
use chrono::{DateTime, Utc};
use config_version::Versioned;
use db::extension_service as db_extension_service;
use eyre::eyre;
use itertools::Itertools;
use model::extension_service::{
    DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DpfHelmChartIdentity, ExtensionServiceType,
};
use model::instance::config::extension_services::InstanceExtensionServiceConfig;
use model::instance::snapshot::InstanceSnapshot;
use model::instance::status::SyncState;
use model::instance::status::extension_service::{
    ExtensionServiceDeploymentStatus, ExtensionServiceStatusObservation,
    InstanceExtensionServiceStatusObservation, InstanceExtensionServicesStatus,
};
use model::machine::ManagedHostStateSnapshot;
use sqlx::PgConnection;
use state_controller::state_handler::StateHandlerError;

use crate::dpf::DpfOperations;

/// Builds instance extension-service status from its two authoritative sources.
///
/// Kubernetes Pod services retain their agent-reported status. DPF Helm services
/// deliberately do not: DPF has no per-DPU, per-service workload observation.
/// Instead, this reports the placement labels persisted on DPUDevice CRs. It
/// must not be interpreted as DPU-cluster Node-label or Helm-workload health.
/// If no DPF SDK is available, reconciliation is skipped and existing
/// observations remain authoritative; no observation is written just because
/// the SDK is unavailable.
pub(super) async fn get_extension_services_status(
    mh_snapshot: &ManagedHostStateSnapshot,
    instance: &InstanceSnapshot,
    db_pool: &sqlx::PgPool,
    dpf_sdk: Option<&dyn DpfOperations>,
) -> Result<InstanceExtensionServicesStatus, StateHandlerError> {
    // An instance deletion is the only force-detach path. Keep the timestamp
    // rather than a separate mode so the derived status can report precisely
    // when every service became removed without changing durable config.
    let instance_deleted_at = instance.deleted.as_ref();
    let service_types = get_extension_service_types_for_instance(instance, db_pool).await?;
    let dpf_service_configs = instance
        .config
        .extension_services
        .service_configs
        .iter()
        .filter(|config| {
            service_types.get(&config.service_id) == Some(&ExtensionServiceType::DpfHelmChart)
        })
        .collect_vec();

    // Derive network-targeted DPUs once. Kubernetes Pod status can tolerate an
    // unavailable mapping, but live DPF placement must not proceed without it.
    let used_dpus = match mh_snapshot.host_snapshot.get_dpu_device_and_id_mappings() {
        Ok((_, device_to_id_map)) => instance.config.network.get_used_dpus(
            &device_to_id_map,
            mh_snapshot.host_snapshot.primary_attached_dpu_machine_id(),
        ),
        Err(error)
            if instance_deleted_at.is_none()
                && !dpf_service_configs.is_empty()
                && dpf_sdk.is_some() =>
        {
            return Err(StateHandlerError::GenericError(eyre!("{error}")));
        }
        Err(_) => vec![],
    };

    let mut observations = instance.observations.extension_services.clone();

    if !dpf_service_configs.is_empty()
        && let Some(dpf_sdk) = dpf_sdk
    {
        let target_dpu_ids = if instance_deleted_at.is_some() {
            HashSet::new()
        } else {
            used_dpus.iter().copied().collect()
        };
        let placement_observations = reconcile_dpf_helm_chart_placement(
            mh_snapshot,
            instance.extension_services_config_version,
            &dpf_service_configs,
            &target_dpu_ids,
            instance_deleted_at,
            dpf_sdk,
            db_pool,
        )
        .await?;

        for (machine_id, observation) in placement_observations {
            observations
                .entry(machine_id)
                .or_default()
                .set_for_service_type(ExtensionServiceType::DpfHelmChart, observation);
        }
    }

    let all_dpus = mh_snapshot
        .dpu_snapshots
        .iter()
        .map(|dpu| dpu.dpu_machine_id())
        .collect::<Result<Vec<_>, _>>()?;
    let dpf_helm_chart_dpus = if instance_deleted_at.is_some() {
        all_dpus
    } else {
        used_dpus.clone()
    };

    Ok(
        InstanceExtensionServicesStatus::from_config_and_service_type_observations(
            Versioned::new(
                &instance.config.extension_services,
                instance.extension_services_config_version,
            ),
            &service_types,
            &used_dpus,
            &dpf_helm_chart_dpus,
            instance_deleted_at,
            &observations,
        ),
    )
}

/// Looks up the persisted service type for every service referenced by an
/// instance configuration. Type is deliberately resolved from the database,
/// not from agent observations, so a DPF service can never accidentally enter
/// the legacy agent-status path.
pub(super) async fn get_extension_service_types_for_instance(
    instance: &InstanceSnapshot,
    db_pool: &sqlx::PgPool,
) -> Result<HashMap<ExtensionServiceId, ExtensionServiceType>, StateHandlerError> {
    let service_ids = instance
        .config
        .extension_services
        .service_configs
        .iter()
        .map(|config| config.service_id)
        .unique()
        .collect_vec();
    let services = {
        let mut connection = db_pool.acquire().await?;
        db_extension_service::find_by_ids(&mut connection, &service_ids, false, false).await?
    };
    let service_types: HashMap<_, _> = services
        .into_iter()
        .map(|service| (service.id, service.service_type))
        .collect();
    if service_ids
        .iter()
        .any(|service_id| !service_types.contains_key(service_id))
    {
        return Err(StateHandlerError::MissingData {
            object_id: instance.id.to_string(),
            missing: "extension service referenced by instance configuration",
        });
    }
    Ok(service_types)
}

/// Reconciles only NICo-owned DPF Helm placement labels for this instance.
///
/// Every physical DPU on the host receives a patch: currently targeted DPUs
/// get each active DPF service's generated label, while non-targeted DPUs and
/// services marked `removed` have that same label deleted.  Applying the
/// complete per-service delta to every physical DPU handles ordinary
/// attachment, detachment, and target-set changes without touching labels
/// owned by DPF or other controllers.
///
/// The caller resolves service types before entering this function, so no
/// database transaction is held across a DPF request. External failures are
/// recorded for required DPUs and do not stop later DPUs from being reconciled.
pub(super) async fn reconcile_dpf_helm_chart_placement(
    mh_snapshot: &ManagedHostStateSnapshot,
    extension_services_config_version: config_version::ConfigVersion,
    dpf_service_configs: &[&InstanceExtensionServiceConfig],
    target_dpu_ids: &HashSet<DpuMachineId>,
    instance_deleted_at: Option<&DateTime<Utc>>,
    dpf_sdk: &dyn DpfOperations,
    db_pool: &sqlx::PgPool,
) -> Result<HashMap<DpuMachineId, InstanceExtensionServiceStatusObservation>, StateHandlerError> {
    if dpf_service_configs.is_empty() {
        return Ok(HashMap::new());
    }
    if !mh_snapshot.host_snapshot.config.dpf.used_for_ingestion {
        return Err(StateHandlerError::GenericError(eyre!(
            "a DPF helm chart extension service is attached to a host that is not DPF-managed"
        )));
    }

    let mut observations = HashMap::new();
    let mut ignored_non_target_failure_count = 0;
    for dpu in &mh_snapshot.dpu_snapshots {
        let dpu_id = dpu.dpu_machine_id()?;
        let is_target = target_dpu_ids.contains(&dpu_id);
        let is_required = is_target || instance_deleted_at.is_some();
        let label_reconciliation = match dpu.dpf_id() {
            None => {
                Err("cannot reconcile DPF helm chart placement: cannot find DPU dpf_id".to_string())
            }
            Some(dpu_device_name) => {
                let changes =
                    dpf_helm_chart_placement_label_changes(dpf_service_configs, is_target);
                let requires_device = is_target
                    && dpf_service_configs
                        .iter()
                        .any(|config| config.removed.is_none());
                match dpf_sdk
                    .merge_dpu_device_node_labels(&dpu_device_name, changes)
                    .await
                {
                    // An absent DPUDevice already has no NICo placement label, so it
                    // satisfies a detach/non-target cleanup. It must remain an error
                    // for an active target because NICo cannot claim placement there.
                    Err(error) if !requires_device && error.is_not_found() => Ok(BTreeMap::new()),
                    Ok(()) => match dpf_sdk.get_dpu_device_node_labels(&dpu_device_name).await {
                        Ok(labels) => Ok(labels),
                        Err(error) if !requires_device && error.is_not_found() => {
                            Ok(BTreeMap::new())
                        }
                        Err(error) => Err(format!(
                            "failed to read DPF helm chart placement labels: {error}"
                        )),
                    },
                    Err(error) => Err(format!(
                        "failed to update DPF helm chart placement labels: {error}"
                    )),
                }
            }
        };

        let evidence = match &label_reconciliation {
            Ok(labels) if is_required => PlacementEvidence::Verified(labels),
            Ok(_) => {
                // Non-target DPUs are reconciled for eventual label cleanup,
                // but are not part of the instance status contract.
                continue;
            }
            Err(message) if is_required => {
                tracing::warn!(dpu_machine_id = %dpu.id, %message, "DPF helm chart placement reconciliation failed");
                PlacementEvidence::Error(message)
            }
            Err(_) => {
                ignored_non_target_failure_count += 1;
                continue;
            }
        };

        let observation = persist_dpf_helm_chart_placement_observation(
            dpu_id,
            extension_services_config_version,
            dpf_service_configs,
            is_target,
            instance_deleted_at,
            evidence,
            db_pool,
        )
        .await?;
        observations.insert(dpu_id, observation);
    }

    if ignored_non_target_failure_count > 0 {
        // Emit one debug record per reconciliation pass rather than one per
        // non-target DPU: cleanup remains eventually consistent without
        // creating noisy logs or affecting the instance lifecycle.
        tracing::debug!(
            ignored_non_target_failure_count,
            "DPF helm chart placement reconciliation failed on non-targeted DPUs; failures will be retried without affecting instance status"
        );
    }

    Ok(observations)
}

/// What a reconciliation pass learned about one DPU's placement labels.
///
/// Keeping each case in one type means a state can never be reported without
/// recording the evidence used to derive it.
#[derive(Clone, Copy)]
enum PlacementEvidence<'a> {
    /// The DPUDevice was read back after its patch, so each service's state is
    /// derived from its persisted placement labels.
    Verified(&'a BTreeMap<String, String>),
    /// A placement write or read failed for this DPU.
    Error(&'a str),
}

/// Persists one DPUDevice placement observation.
///
/// The write is intentionally per DPU rather than batched at the end of the
/// pass, so a failure on a later DPU cannot discard the verified results of
/// DPUs this pass already reconciled.
async fn persist_dpf_helm_chart_placement_observation(
    dpu_id: DpuMachineId,
    config_version: config_version::ConfigVersion,
    dpf_service_configs: &[&InstanceExtensionServiceConfig],
    is_target: bool,
    instance_deleted_at: Option<&DateTime<Utc>>,
    evidence: PlacementEvidence<'_>,
    db_pool: &sqlx::PgPool,
) -> Result<InstanceExtensionServiceStatusObservation, StateHandlerError> {
    let observed_at = Utc::now();
    let observation = dpf_helm_chart_placement_observation(
        config_version,
        dpf_service_configs,
        is_target,
        instance_deleted_at,
        evidence,
        observed_at,
    );

    let mut txn = db_pool.begin().await?;
    let applied = db::machine::update_extension_service_status_observation(
        txn.as_mut(),
        &dpu_id,
        ExtensionServiceType::DpfHelmChart,
        &observation,
    )
    .await?;
    txn.commit().await?;

    warn_if_superseded(applied, dpu_id, observed_at);

    Ok(observation)
}

/// A rejected write means another writer stored a newer observation for this
/// DPU, so the caller is racing a concurrent reconciliation of the same host.
fn warn_if_superseded(applied: bool, dpu_id: DpuMachineId, observed_at: chrono::DateTime<Utc>) {
    if !applied {
        tracing::warn!(
            dpu_machine_id = %dpu_id,
            %observed_at,
            "a newer DPF Helm chart placement observation already exists; discarding this one"
        );
    }
}

/// Builds one DPU's placement observation from the desired attachment and the
/// evidence gathered for it. `Running` means the placement label is persisted
/// on the DPUDevice, not that the Helm workload itself is healthy.
fn dpf_helm_chart_placement_observation(
    config_version: config_version::ConfigVersion,
    dpf_service_configs: &[&InstanceExtensionServiceConfig],
    is_target: bool,
    instance_deleted_at: Option<&DateTime<Utc>>,
    evidence: PlacementEvidence<'_>,
    observed_at: chrono::DateTime<Utc>,
) -> InstanceExtensionServiceStatusObservation {
    let extension_service_statuses = dpf_service_configs
        .iter()
        .map(|config| {
            let identity = DpfHelmChartIdentity::from_service_id(config.service_id);
            let placement_is_desired =
                instance_deleted_at.is_none() && config.removed.is_none() && is_target;
            let (overall_state, message) = match evidence {
                PlacementEvidence::Verified(labels) if placement_is_desired => (
                    if labels
                        .get(&identity.placement_label_key)
                        .is_some_and(|value| value == DPF_HELM_CHART_PLACEMENT_LABEL_VALUE)
                    {
                        ExtensionServiceDeploymentStatus::Running
                    } else {
                        ExtensionServiceDeploymentStatus::Pending
                    },
                    String::new(),
                ),
                PlacementEvidence::Verified(labels) => (
                    if labels.contains_key(&identity.placement_label_key) {
                        ExtensionServiceDeploymentStatus::Terminating
                    } else {
                        ExtensionServiceDeploymentStatus::Terminated
                    },
                    String::new(),
                ),
                PlacementEvidence::Error(message) => {
                    (ExtensionServiceDeploymentStatus::Error, message.to_owned())
                }
            };
            ExtensionServiceStatusObservation {
                service_id: config.service_id,
                service_type: ExtensionServiceType::DpfHelmChart,
                service_name: String::new(),
                version: config.version,
                removed: config
                    .removed
                    .as_ref()
                    .or(instance_deleted_at)
                    .map(ToString::to_string),
                overall_state,
                components: vec![],
                message,
            }
        })
        .collect();

    InstanceExtensionServiceStatusObservation {
        config_version,
        instance_config_version: None,
        extension_service_statuses,
        observed_at,
    }
}

/// Builds the NICo-owned label changes for one physical DPU.
///
/// An active DPF Helm chart service is enabled only when this DPU is currently
/// targeted by the instance network configuration. Removed services, and
/// active services on a DPU removed from that target set, are represented by a
/// `None` value so the DPUDevice merge patch deletes only that service's
/// placement label.
pub(super) fn dpf_helm_chart_placement_label_changes(
    dpf_service_configs: &[&InstanceExtensionServiceConfig],
    is_target: bool,
) -> BTreeMap<String, Option<String>> {
    dpf_service_configs
        .iter()
        .map(|config| {
            let identity = DpfHelmChartIdentity::from_service_id(config.service_id);
            let value = if config.removed.is_none() && is_target {
                Some(DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_string())
            } else {
                None
            };
            (identity.placement_label_key, value)
        })
        .collect()
}

pub(super) async fn cleanup_terminated_extension_services(
    instance: &InstanceSnapshot,
    extension_services_status: &mut InstanceExtensionServicesStatus,
    txn: &mut PgConnection,
) -> Result<(), StateHandlerError> {
    if extension_services_status.configs_synced != SyncState::Synced {
        return Ok(());
    }

    let terminated_service_keys = extension_services_status.get_terminated_service_keys();
    if terminated_service_keys.is_empty() {
        return Ok(());
    }

    tracing::info!(
        instance_id = %instance.id,
        terminated_extension_services = ?terminated_service_keys,
        "Cleaning up fully terminated extension services from instance config"
    );
    let new_config = instance
        .config
        .extension_services
        .remove_terminated_services(&terminated_service_keys);

    db::instance::update_extension_services_config(
        txn,
        instance.id,
        instance.extension_services_config_version,
        &new_config,
        false,
    )
    .await?;

    extension_services_status.extension_services.retain(|svc| {
        !terminated_service_keys
            .iter()
            .any(|&(id, ver)| id == svc.service_id && ver == svc.version)
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::str::FromStr;

    use carbide_uuid::extension_service::ExtensionServiceId;
    use chrono::Utc;
    use config_version::ConfigVersion;

    use super::PlacementEvidence::Verified;
    use super::*;

    #[test]
    fn dpf_helm_placement_changes_cover_attach_detach_and_target_changes() {
        let active_service =
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
        let removed_service =
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000002").unwrap();
        let version = ConfigVersion::initial();
        let active = InstanceExtensionServiceConfig {
            service_id: active_service,
            version,
            removed: None,
        };
        let removed = InstanceExtensionServiceConfig {
            service_id: removed_service,
            version,
            removed: Some(Utc::now()),
        };
        let configs = vec![&active, &removed];
        let active_label =
            DpfHelmChartIdentity::from_service_id(active_service).placement_label_key;
        let removed_label =
            DpfHelmChartIdentity::from_service_id(removed_service).placement_label_key;

        assert_eq!(
            dpf_helm_chart_placement_label_changes(&configs, true),
            BTreeMap::from([
                (
                    active_label.clone(),
                    Some(DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_string()),
                ),
                (removed_label.clone(), None),
            ])
        );
        assert_eq!(
            dpf_helm_chart_placement_label_changes(&configs, false),
            BTreeMap::from([(active_label, None), (removed_label, None)])
        );
    }

    #[test]
    fn dpf_helm_placement_statuses_are_derived_from_live_labels() {
        let active_service =
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
        let removed_service =
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000002").unwrap();
        let version = ConfigVersion::initial();
        let active = InstanceExtensionServiceConfig {
            service_id: active_service,
            version,
            removed: None,
        };
        let removed = InstanceExtensionServiceConfig {
            service_id: removed_service,
            version,
            removed: Some(Utc::now()),
        };
        let configs = vec![&active, &removed];
        let active_label =
            DpfHelmChartIdentity::from_service_id(active_service).placement_label_key;
        let removed_label =
            DpfHelmChartIdentity::from_service_id(removed_service).placement_label_key;

        let labels = BTreeMap::from([
            (
                active_label,
                DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_string(),
            ),
            (
                removed_label,
                DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_string(),
            ),
        ]);
        let states: Vec<_> = dpf_helm_chart_placement_observation(
            version,
            &configs,
            true,
            None,
            Verified(&labels),
            Utc::now(),
        )
        .extension_service_statuses
        .into_iter()
        .map(|status| status.overall_state)
        .collect();
        assert_eq!(
            states,
            vec![
                ExtensionServiceDeploymentStatus::Running,
                ExtensionServiceDeploymentStatus::Terminating,
            ]
        );

        let instance_deleted_at = Utc::now();
        let states: Vec<_> = dpf_helm_chart_placement_observation(
            version,
            &configs,
            false,
            Some(&instance_deleted_at),
            Verified(&BTreeMap::new()),
            Utc::now(),
        )
        .extension_service_statuses
        .into_iter()
        .map(|status| status.overall_state)
        .collect();
        assert_eq!(
            states,
            vec![
                ExtensionServiceDeploymentStatus::Terminated,
                ExtensionServiceDeploymentStatus::Terminated,
            ]
        );
    }

    #[test]
    fn dpf_helm_placement_error_is_reported_for_every_service_on_the_failed_dpu() {
        let first_service =
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
        let second_service =
            ExtensionServiceId::from_str("00000000-0000-0000-0000-000000000002").unwrap();
        let version = ConfigVersion::initial();
        let first = InstanceExtensionServiceConfig {
            service_id: first_service,
            version,
            removed: None,
        };
        let second = InstanceExtensionServiceConfig {
            service_id: second_service,
            version,
            removed: None,
        };
        let configs = vec![&first, &second];

        let statuses = dpf_helm_chart_placement_observation(
            version,
            &configs,
            true,
            None,
            PlacementEvidence::Error("failed to update DPF helm chart placement labels"),
            Utc::now(),
        )
        .extension_service_statuses;

        assert_eq!(statuses.len(), 2);
        for status in statuses {
            assert_eq!(
                status.overall_state,
                ExtensionServiceDeploymentStatus::Error
            );
            assert_eq!(
                status.message,
                "failed to update DPF helm chart placement labels"
            );
        }
    }
}
