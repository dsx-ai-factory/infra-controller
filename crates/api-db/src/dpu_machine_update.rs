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
use model::dpu_machine_update::DpuMachineUpdate;
use model::machine::{HostHealthConfig, LoadSnapshotOptions, ManagedHostState, ReprovisionRequest};
use model::machine_update_module::{
    AutomaticFirmwareUpdateReference, DPU_FIRMWARE_UPDATE_TARGET, DpuReprovisionInitiator,
    HOST_UPDATE_HEALTH_PROBE_ID, HOST_UPDATE_HEALTH_REPORT_SOURCE,
};
use sqlx::PgConnection;

use crate::{DatabaseError, Transaction};

pub async fn get_fw_updates_running_count(txn: &mut PgConnection) -> Result<i64, DatabaseError> {
    let query = r#"SELECT COUNT(*) as count FROM machines m
            WHERE (reprovisioning_requested->>'update_firmware')::boolean is true
            AND reprovisioning_requested->>'started_at' IS NOT NULL;"#;
    let (count,): (i64,) = sqlx::query_as(query)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("get_fw_updates_running_count", e))?;

    Ok(count)
}

pub async fn trigger_reprovisioning_for_managed_host(
    txn: &mut PgConnection,
    machine_updates: &[DpuMachineUpdate],
) -> Result<(), DatabaseError> {
    let mut inner_txn = Transaction::begin_inner(txn).await?;

    for machine_update in machine_updates {
        let initiator = DpuReprovisionInitiator::Automatic(AutomaticFirmwareUpdateReference {
            from: machine_update.firmware_version.clone(),
            to: "".to_string(),
        });

        let reprovision_time = chrono::Utc::now();
        let req = ReprovisionRequest {
            requested_at: reprovision_time,
            initiator: initiator.to_string(),
            update_firmware: true,
            started_at: None,
            user_approval_received: false,
            restart_reprovision_requested_at: reprovision_time,
        };

        let query = r#"UPDATE machines SET reprovisioning_requested=$1 WHERE controller_state = '{"state": "ready"}' AND id=$2 RETURNING id"#;
        sqlx::query(query)
            .bind(sqlx::types::Json(req))
            .bind(machine_update.dpu_machine_id)
            .fetch_one(inner_txn.as_pgconn())
            .await
            .map_err(|err: sqlx::Error| match err {
                sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                    kind: "trigger_reprovisioning_for_managed_host",
                    id: machine_update.dpu_machine_id.to_string(),
                },
                _ => DatabaseError::query(query, err),
            })?;
    }

    inner_txn.commit().await?;

    Ok(())
}

pub async fn get_reprovisioning_machines(
    txn: &mut PgConnection,
) -> Result<Vec<DpuMachineUpdate>, DatabaseError> {
    let reference = AutomaticFirmwareUpdateReference::REF_NAME.to_string() + "%";

    let query = r#"SELECT mi.machine_id AS host_machine_id, m.id AS dpu_machine_id, '' AS firmware_version
            FROM machines m
            INNER JOIN machine_interfaces mi ON m.id = mi.attached_dpu_machine_id
            WHERE m.reprovisioning_requested->>'initiator' like $1
            AND mi.interface_type != 'Bmc'
            AND mi.attached_dpu_machine_id != mi.machine_id;"#;

    let result: Vec<DpuMachineUpdate> = sqlx::query_as(query)
        .bind(&reference)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(result)
}

pub async fn get_updated_machines(
    txn: &mut PgConnection,
    host_health_config: HostHealthConfig,
) -> Result<Vec<DpuMachineUpdate>, DatabaseError> {
    let machine_ids = crate::managed_host::load_host_ids(&mut *txn).await?;
    let snapshots = crate::managed_host::load_by_machine_ids(
        txn,
        &machine_ids,
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config,
        },
    )
    .await?;

    let updated_machines: Vec<DpuMachineUpdate> = snapshots
        .into_iter()
        .filter_map(|(machine_id, managed_host)| {
            // Skip looking at any machines that are not marked for updates
            if !managed_host
                .host_snapshot
                .health_reports
                .merges
                .get(HOST_UPDATE_HEALTH_REPORT_SOURCE)
                .is_some_and(|updater_report| {
                    updater_report.alerts.iter().any(|alert| {
                        alert.id == *HOST_UPDATE_HEALTH_PROBE_ID
                            && alert.target.as_deref() == Some(DPU_FIRMWARE_UPDATE_TARGET)
                    })
                })
            {
                return None;
            }
            // Skip any machines that are not done updating
            if !matches!(managed_host.managed_state, ManagedHostState::Ready) {
                return None;
            }
            // Check if all DPUs have the `reprovisioning_requested` flag cleared
            if managed_host
                .dpu_snapshots
                .iter()
                .any(|dpu| dpu.reprovision_requested.is_some())
            {
                return None;
            }

            let dpf_managed = managed_host.host_snapshot.config.dpf.used_for_ingestion;

            // We only signal an update as complete once ALL DPUs are done
            // That prevents removing the updating flags from the Host
            // if just one DPU completes the update
            let completed_updates: Result<Vec<DpuMachineUpdate>, DatabaseError> = managed_host
                .dpu_snapshots
                .iter()
                .map(|dpu| {
                    Ok(DpuMachineUpdate {
                        host_machine_id: machine_id,
                        dpu_machine_id: dpu.dpu_machine_id()?,
                        firmware_version: dpu
                            .status
                            .hardware_info
                            .as_ref()
                            .and_then(|info| info.dpu_info.as_ref())
                            .map(|dpu_info| dpu_info.firmware_version.clone())
                            .unwrap_or_default(),
                        dpf_managed,
                    })
                })
                .collect();

            Some(completed_updates)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();

    Ok(updated_machines)
}
