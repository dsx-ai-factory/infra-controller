// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use carbide_uuid::machine::DpuMachineId;
use serde_json::json;

use super::{DpfSdk, dpu_device_cr_name};
use crate::DpfError;
use crate::repository::DpuDeviceRepository;
use crate::types::DPU_MACHINE_ID_LABEL;

// Use ten seconds per management attempt, matching the operator-version lookup, including
// client capacity waits. A backfill uses at most two attempts (twenty seconds).
// There is no automatic retry; the caller rereads after an uncertain patch outcome.
const NODE_IDENTITY_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

impl<R: DpuDeviceRepository, L> DpfSdk<R, L> {
    /// Backfills a missing declared Node identity without reassigning an existing DPU identity.
    ///
    /// Callers must pair the raw device ID (without the device- prefix) with its persisted DPU
    /// machine ID. A missing or deleting DPUDevice, conflicting management or Node label, or missing
    /// UID or resourceVersion is an error. Reads and patches each have a ten-second attempt deadline.
    /// Success confirms management acceptance. Callers must separately verify the remote Node.
    pub async fn ensure_dpu_device_node_identity(
        &self,
        dpu_device_name: &str,
        dpu_id: DpuMachineId,
    ) -> Result<(), DpfError> {
        // Read the current device before deciding whether identity propagation needs a patch.
        let name = dpu_device_cr_name(dpu_device_name);
        let device = tokio::time::timeout(
            NODE_IDENTITY_REQUEST_TIMEOUT,
            DpuDeviceRepository::get(&*self.repo, &name, &self.namespace),
        )
        .await
        .map_err(|_| {
            DpfError::timeout(
                "read DPUDevice Node identity",
                "ten-second attempt deadline exceeded",
            )
        })??
        .ok_or_else(|| DpfError::not_found("DPUDevice", &name))?;
        let expected = dpu_id.to_string();
        let management_identity = device
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(DPU_MACHINE_ID_LABEL));
        let node_identity = device
            .spec
            .cluster
            .as_ref()
            .and_then(|cluster| cluster.node_labels.as_ref())
            .and_then(|labels| labels.get(DPU_MACHINE_ID_LABEL));

        // Refuse reassignment even if one of the two existing identity labels still matches.
        if device.metadata.deletion_timestamp.is_some()
            || management_identity.is_some_and(|identity| identity != &expected)
            || node_identity.is_some_and(|identity| identity != &expected)
        {
            return Err(DpfError::InvalidState(format!(
                "DPUDevice {name} is deleting or has a different DPU machine identity"
            )));
        }
        // Require complete device identity before accepting or patching its Node label.
        let uid = device
            .metadata
            .uid
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DpfError::InvalidState(format!("DPUDevice {name} has no metadata.uid"))
            })?;
        let resource_version = device
            .metadata
            .resource_version
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DpfError::InvalidState(format!("DPUDevice {name} has no metadata.resourceVersion"))
            })?;
        if node_identity == Some(&expected) {
            return Ok(());
        }

        // Fence the backfill against device replacement and intervening cluster-label changes.
        let patch = json!({
            "metadata": {"uid": uid, "resourceVersion": resource_version},
            "spec": {"cluster": {"nodeLabels": {DPU_MACHINE_ID_LABEL: expected}}},
        });
        tokio::time::timeout(
            NODE_IDENTITY_REQUEST_TIMEOUT,
            DpuDeviceRepository::patch(&*self.repo, &name, &self.namespace, patch),
        )
        .await
        .map_err(|_| {
            DpfError::timeout(
                "patch DPUDevice Node identity",
                "ten-second attempt deadline exceeded",
            )
        })?
    }
    /// Reads a non-deleting DPUDevice with its labels, UID, generation, and resource
    /// version so a subsequent placement patch cannot overwrite a newer withdrawal or replacement.
    ///
    /// `Some(expected_dpu_id)` additionally requires a matching declared Node identity.
    /// Callers must pass `Some` for service VPC endpoint DPUs and `None` only for label-only
    /// placement on DPUs without service VPC endpoints.
    pub async fn get_dpu_device_placement(
        &self,
        dpu_device_name: &str,
        expected_dpu_id: Option<DpuMachineId>,
    ) -> Result<crate::DpuDevicePlacementObservation, DpfError> {
        let name = dpu_device_cr_name(dpu_device_name);
        let device = super::bounded_management_request(
            "read DPUDevice placement",
            DpuDeviceRepository::get(&*self.repo, &name, &self.namespace),
        )
        .await?
        .ok_or_else(|| DpfError::not_found("DPUDevice", &name))?;
        let labels = device
            .spec
            .cluster
            .and_then(|cluster| cluster.node_labels)
            .unwrap_or_default();

        // Require matching Node identity for service VPC endpoints while retaining label-only placement elsewhere.
        if device.metadata.deletion_timestamp.is_some()
            || expected_dpu_id
                .is_some_and(|dpu_id| labels.get(DPU_MACHINE_ID_LABEL) != Some(&dpu_id.to_string()))
        {
            return Err(DpfError::InvalidState(format!(
                "DPUDevice {name} is deleting or has no matching DPU identity"
            )));
        }
        let uid = device
            .metadata
            .uid
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DpfError::InvalidState(format!("DPUDevice {name} has no metadata.uid"))
            })?;
        let resource_version = device
            .metadata
            .resource_version
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DpfError::InvalidState(format!("DPUDevice {name} has no metadata.resourceVersion"))
            })?;
        let generation = device.metadata.generation.ok_or_else(|| {
            DpfError::InvalidState(format!("DPUDevice {name} has no metadata.generation"))
        })?;
        Ok(crate::DpuDevicePlacementObservation {
            resource: crate::ServiceVpcResourceObservation {
                namespace: self.namespace.clone(),
                name,
                uid,
                generation,
                resource_version,
                is_deleting: false,
            },
            node_labels: labels,
            config_version: device.metadata.annotations.and_then(|mut annotations| {
                annotations.remove("nico/service-vpc-placement-config")
            }),
        })
    }

    /// Applies only the supplied label delta to the exact observed DPUDevice so an older active
    /// write cannot overwrite a later withdrawal or bind a replacement device.
    ///
    /// Callers must revalidate committed attachment desire after observing the supplied revision
    /// and reread placement after success. Each attempt has a ten-second deadline and no automatic retry.
    pub async fn patch_dpu_device_placement(
        &self,
        dpu_device_name: &str,
        changes: std::collections::BTreeMap<String, Option<String>>,
        config_version: &str,
        preconditions: &crate::DpfResourcePreconditions,
    ) -> Result<(), DpfError> {
        // Keep DPU identity immutable and refuse an unfenced placement mutation.
        if config_version.is_empty()
            || changes.contains_key(DPU_MACHINE_ID_LABEL)
            || preconditions.uid.is_empty()
            || preconditions.resource_version.is_empty()
        {
            return Err(DpfError::InvalidState(
                "placement requires an observed UID and resource version and cannot modify DPU identity".to_owned(),
            ));
        }
        // Stamp the configuration even when labels are already absent so withdrawal advances
        // the resource version and rejects any delayed active write prepared before it.
        let name = dpu_device_cr_name(dpu_device_name);
        let patch = json!({
            "metadata": {
                "uid": preconditions.uid,
                "resourceVersion": preconditions.resource_version,
                "annotations": {"nico/service-vpc-placement-config": config_version},
            },
            "spec": {"cluster": {"nodeLabels": changes}},
        });
        super::bounded_management_request(
            "patch DPUDevice placement",
            DpuDeviceRepository::patch(&*self.repo, &name, &self.namespace, patch),
        )
        .await
    }
}

#[cfg(test)]
mod tests;
