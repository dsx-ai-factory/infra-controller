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

use chrono::Utc;
use prettytable::row;
use rpc::forge::{
    DpuServiceSyncReleaseStatus, PendingDpuServiceSync, ReleaseDpuServiceSyncHoldRequest,
    UpdateInitiator,
};

use super::args::{Args, List, Release};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn service_sync(
    api_client: &ApiClient,
    args: Args,
    page_size: usize,
) -> CarbideCliResult<()> {
    match args {
        Args::List(args) => list(api_client, args, page_size).await,
        Args::Release(args) => release(api_client, args).await,
    }
}

async fn list(api_client: &ApiClient, args: List, page_size: usize) -> CarbideCliResult<()> {
    let pending = match args.machine_id {
        Some(machine_id) => api_client.list_dpu_service_sync_history(machine_id).await?,
        None => api_client.list_pending_dpu_service_syncs(page_size).await?,
    };

    if pending.is_empty() {
        match args.machine_id {
            Some(machine_id) => println!("No recorded DPU service syncs for {machine_id}"),
            None => println!("No machines are waiting on a DPUService rollout"),
        }
        return Ok(());
    }

    // The single-machine form is a history, so it is worth different columns:
    // when each sync completed and who completed it, rather than how long the
    // fleet has been waiting.
    if args.machine_id.is_some() {
        print_history(&pending);
    } else {
        print_worklist(&pending);
    }
    Ok(())
}

fn print_worklist(pending: &[PendingDpuServiceSync]) {
    let mut table = prettytable::Table::new();
    table.set_titles(row![
        "Machine Id",
        "State",
        "Waiting Since",
        "Age",
        "Instance"
    ]);
    for entry in pending {
        let requested_at = entry.requested_at.unwrap_or_default();
        table.add_row(row![
            entry.machine_id.unwrap_or_default().to_string(),
            entry.state,
            requested_at,
            age(entry),
            entry
                .instance_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ]);
    }
    table.printstd();
}

fn print_history(pending: &[PendingDpuServiceSync]) {
    let mut table = prettytable::Table::new();
    table.set_titles(row!["Requested At", "Completed At", "Completed By"]);
    for entry in pending {
        let completed_at = entry
            .completed_at
            .map(|at| at.to_string())
            .unwrap_or_else(|| "outstanding".to_string());
        let completed_by = match entry.completed_by {
            Some(_) => match entry.completed_by() {
                UpdateInitiator::AdminCli => "operator",
                UpdateInitiator::Automatic => "nico",
            },
            None => "-",
        };
        table.add_row(row![
            entry.requested_at.unwrap_or_default(),
            completed_at,
            completed_by,
        ]);
    }
    table.printstd();
}

/// How long the machine has been waiting, which is the column an operator
/// actually scans. Computed here rather than server-side so it stays honest
/// about when the list was read.
fn age(entry: &PendingDpuServiceSync) -> String {
    let Some(requested_at) = entry.requested_at else {
        return "-".to_string();
    };
    let Ok(requested_at) = chrono::DateTime::try_from(requested_at) else {
        return "-".to_string();
    };
    let elapsed = Utc::now().signed_duration_since(requested_at);
    if elapsed.num_days() > 0 {
        format!("{}d{}h", elapsed.num_days(), elapsed.num_hours() % 24)
    } else if elapsed.num_hours() > 0 {
        format!("{}h{}m", elapsed.num_hours(), elapsed.num_minutes() % 60)
    } else {
        format!("{}m", elapsed.num_minutes().max(0))
    }
}

async fn release(api_client: &ApiClient, args: Release) -> CarbideCliResult<()> {
    let request = ReleaseDpuServiceSyncHoldRequest::from(args);
    let results = api_client.release_dpu_service_sync_hold(request).await?;

    let mut table = prettytable::Table::new();
    table.set_titles(row!["Machine Id", "Result", "Detail"]);
    let (mut released, mut deferred, mut failed, mut not_pending) = (0, 0, 0, 0);
    for result in &results {
        let status = result.status();
        match status {
            DpuServiceSyncReleaseStatus::Released => released += 1,
            DpuServiceSyncReleaseStatus::Failed => failed += 1,
            DpuServiceSyncReleaseStatus::NotPending => not_pending += 1,
            _ => deferred += 1,
        }
        table.add_row(row![
            result.machine_id.unwrap_or_default().to_string(),
            describe(status),
            result.detail,
        ]);
    }
    table.printstd();
    // Counts every row printed above. Omitting the already-handled ones made a
    // repeat run -- the most common invocation, since that is what makes this
    // safe to loop -- report all zeros beneath a full table.
    println!(
        "{released} released, {deferred} deferred, {not_pending} already handled, {failed} failed"
    );

    // Only a failure is an error. A deferral is the documented answer and
    // "not pending" is what a repeat run reports, so treating either as failure
    // would break any drain-until-clean loop.
    if failed > 0 {
        return Err(CarbideCliError::GenericError(format!(
            "{failed} machine(s) could not be released; see the Detail column"
        )));
    }
    Ok(())
}

fn describe(status: DpuServiceSyncReleaseStatus) -> &'static str {
    match status {
        DpuServiceSyncReleaseStatus::Released => "released",
        DpuServiceSyncReleaseStatus::NotPending => "not pending",
        DpuServiceSyncReleaseStatus::DeferredDpuOutdated => "deferred: DPU awaits reprovisioning",
        DpuServiceSyncReleaseStatus::DeferredHostAssigned => "deferred: host assigned",
        DpuServiceSyncReleaseStatus::DeferredUnknown => "deferred: DPU could not be evaluated",
        DpuServiceSyncReleaseStatus::Failed => "failed",
        DpuServiceSyncReleaseStatus::Unspecified => "unknown",
    }
}
