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
    /// machine ID. A conflicting management or Node label, deleting device, or missing patch
    /// precondition is an error. Reads and patches each have a ten-second attempt deadline.
    /// Success confirms management acceptance; callers must separately verify the remote Node.
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
}

#[cfg(test)]
mod tests;
