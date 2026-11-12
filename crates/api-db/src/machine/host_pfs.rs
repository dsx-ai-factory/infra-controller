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

use super::*;

/// Detects initial BF4 host states from current DPU topology and retained agent classification so implicit boot publishers defer provisional targets.
///
/// Callers that act on this answer must serialize their write on the machine group and re-read after acquiring those locks.
pub async fn bf4_host_pf_initialization_pending(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
) -> DatabaseResult<bool> {
    if !matches!(
        lookup_managed_host_state(&mut *txn, *host_id.as_machine_id()).await?,
        Some(
            ManagedHostState::Created
                | ManagedHostState::ConfigureAstra { .. }
                | ManagedHostState::DPUInit { .. }
                | ManagedHostState::DpuDiscoveringState { .. }
        )
    ) {
        return Ok(false);
    }
    host_has_bf4_dpu(txn, host_id).await
}

/// Detects attached BF4 hardware or retained agent classification independently of lifecycle state so boot-NIC adoption can preserve DPU networking.
///
/// Callers changing interface ownership or primary flags must retain their transaction's segment and interface locks.
pub async fn host_has_bf4_dpu(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
) -> DatabaseResult<bool> {
    let query = "SELECT DISTINCT attached_dpu_machine_id FROM machine_interfaces
                 WHERE machine_id = $1 AND interface_type != 'Bmc'
                   AND attached_dpu_machine_id IS NOT NULL";
    let dpu_ids: Vec<DpuMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(
        crate::machine_topology::find_latest_by_machine_ids(txn, &dpu_ids)
            .await?
            .values()
            .any(|topology| {
                topology.topology.discovery_data.info.is_bf4_dpu()
                    || topology
                        .dpu_host_pf_inventory
                        .as_ref()
                        .is_some_and(|inventory| inventory.is_bf4)
            }),
    )
}

/// Fences initial HostInit publication against state, network-version, instance and DPU-membership changes while retaining fresh-report locks.
///
/// Callers must perform no interface repair in this transaction, reload DPU ACK/health after success, retain it through the state CAS, and roll back on false.
/// History locks precede machine locks; interface and DPU locks acquired after the host never wait.
pub async fn lock_host_init_network_config(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    expected_state_version: ConfigVersion,
    expected_network_version: ConfigVersion,
    expected_dpus: &[DpuMachineId],
) -> DatabaseResult<bool> {
    // Match controller-state publication's history-before-machine order before taking any row.
    let query = "SELECT pg_advisory_xact_lock(hashtextextended($1, 'machine_state_history'::regclass::bigint))";
    sqlx::query(query)
        .bind(host_id.to_string())
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let query = "SELECT id FROM machines WHERE id = $1
                 AND controller_state_version = $2 AND network_config_version = $3
                 AND controller_state->>'state' = 'dpuinit' FOR UPDATE";
    let locked: Option<HostMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .bind(expected_state_version)
        .bind(expected_network_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if locked.is_none()
        || crate::instance::find_id_by_machine_id(&mut *txn, host_id.as_machine_id())
            .await?
            .is_some()
        || !try_lock_host_interfaces_and_dpus(txn, host_id).await?
    {
        return Ok(false);
    }

    // Compare the locked attachments with the exact DPU set used to settle PF facts.
    let query = "SELECT DISTINCT attached_dpu_machine_id FROM machine_interfaces
                 WHERE machine_id = $1 AND interface_type != 'Bmc'
                   AND attached_dpu_machine_id IS NOT NULL ORDER BY attached_dpu_machine_id";
    let attached: Vec<DpuMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let mut expected = expected_dpus.to_vec();
    expected.sort();
    expected.dedup();
    Ok(attached == expected)
}
