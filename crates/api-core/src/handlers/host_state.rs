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

//! `FindHostStatesByIds`: compact per-host state for monitoring, read from
//! one database snapshot.

use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::Arc;

use ::rpc::forge as rpc;
use ::rpc::model::machine::host_state::{explored_firmware, host_state_from_snapshot};
use carbide_uuid::machine::HostMachineId;
use itertools::Itertools;
use model::host_machine_update::{firmware_policy, host_update_eligibility};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::machine_update_manager::host_on_disable_list;

/// Test hook carried in one request's extensions. The handler notifies
/// `after_snapshot` once the snapshot is read, then waits for `resume` before
/// the gate read.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct SnapshotReadHook {
    pub(crate) after_snapshot: Arc<tokio::sync::Notify>,
    pub(crate) resume: Arc<tokio::sync::Notify>,
}

pub(crate) async fn find_host_states_by_ids(
    api: &Api,
    request: Request<rpc::HostStatesByIdsRequest>,
) -> Result<Response<rpc::HostStateList>, Status> {
    log_request_data(&request);
    #[cfg(test)]
    let hook = request.extensions().get::<SnapshotReadHook>().cloned();
    let request = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if request.host_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if request.host_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }
    // Sorted and deduplicated. DPU IDs are rejected.
    let host_ids: Vec<HostMachineId> = request
        .host_ids
        .into_iter()
        .map(HostMachineId::try_from)
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| CarbideError::InvalidArgument(error.to_string()))?
        .into_iter()
        .collect();

    let config = &api.runtime_config;
    let now = chrono::Utc::now();
    let mut txn = api.txn_begin().await?;
    db::set_read_only_snapshot(&mut txn).await?;
    // The catalog is built per call from config and overrides, as the update
    // manager builds it, so a stale desired_firmware table is never read.
    let overrides = db::host_firmware_config::list_configs(txn.as_pgconn()).await?;
    let catalog = config
        .get_firmware_config()
        .create_snapshot_with_overrides(overrides);
    let mut snapshots =
        db::managed_host::load_compact_by_host_ids(&mut txn, &host_ids, config.host_health).await?;
    #[cfg(test)]
    if let Some(hook) = hook {
        hook.after_snapshot.notify_one();
        hook.resume.notified().await;
    }
    let mut gates = db::host_machine_update::find_update_gates(
        &mut txn,
        now,
        config.firmware_global.autoupdate,
        config.firmware_global.instance_updates_manual_tagging,
        &db::desired_firmware::desired_firmware_rows(catalog.values()),
        &host_ids,
    )
    .await?
    .into_iter()
    .into_group_map_by(|row| row.host_id);
    txn.commit().await?;
    let read_at = chrono::Utc::now();

    let hosts = host_ids
        .iter()
        .filter_map(|host_id| {
            let snapshot = snapshots.remove(host_id)?;
            let rows = gates.remove(host_id).unwrap_or_default();
            let bmc_ip = snapshot.host_snapshot.status.bmc_info.ip;
            let firmware_autoupdate = snapshot.host_snapshot.config.firmware_autoupdate;
            let on_disable_list = host_on_disable_list(&config.firmware_global, host_id);
            let explored = rows
                .iter()
                .find(|row| bmc_ip.is_some() && row.bmc_address == bmc_ip)
                .and_then(|row| {
                    // The model entry the updater uses for this report.
                    let inventory = row
                        .vendor()
                        .zip(row.report_model.as_deref())
                        .and_then(|(vendor, model)| catalog.find(vendor, model))
                        .and_then(|firmware| {
                            firmware.installed_inventory(
                                row.report_service
                                    .as_ref()
                                    .map_or(&[], |services| &services.0),
                            )
                        });
                    explored_firmware(row, inventory)
                });
            Some(host_state_from_snapshot(
                snapshot,
                explored,
                host_update_eligibility(&rows, bmc_ip, firmware_autoupdate, on_disable_list),
                firmware_policy(&rows, firmware_autoupdate, on_disable_list),
            ))
        })
        .collect();

    Ok(Response::new(rpc::HostStateList {
        hosts,
        read_at: Some(read_at.into()),
    }))
}
