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

use eyre::WrapErr;
use prettytable::{Row, Table, row};
use rpc::forge::{ResourcePoolBackendKind, ResourcePoolInspection, ResourcePoolStatsStatus};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::{ApiClient, maybe_unimplemented};

pub(crate) async fn list(data: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let rows: Vec<Row> = match api_client.0.admin_inspect_resource_pools().await {
        Ok(response) => response.pools.into_iter().map(inspection_row).collect(),
        Err(error) if maybe_unimplemented(&error) => {
            // Servers without inspection only report local inventory. Keep their
            // exact totals without inventing the missing partition counts.
            api_client
                .0
                .admin_list_resource_pools(data)
                .await
                .wrap_err("failed to list resource pools on a server without inspection")?
                .pools
                .into_iter()
                .map(|pool| {
                    row![
                        pool.name,
                        "Integrated",
                        pool.name,
                        pool.min,
                        pool.max,
                        pool.total,
                        usage(pool.allocated, pool.total),
                        "Known",
                    ]
                })
                .collect()
        }
        Err(error) => {
            return Err(eyre::Report::from(error)
                .wrap_err("failed to inspect resource pools")
                .into());
        }
    };
    if rows.is_empty() {
        println!("No resource pools defined");
        return Err(CarbideCliError::Empty);
    }

    let mut table = Table::new();
    table.set_titles(row![
        "Name",
        "Backend",
        "Remote Pool",
        "Min",
        "Max",
        "Size",
        "Used",
        "Status"
    ]);
    for row in rows {
        table.add_row(row);
    }
    table.printstd();
    Ok(())
}

/// `inspection_row` keeps unknown counts visible alongside pools with known capacity.
fn inspection_row(pool: ResourcePoolInspection) -> Row {
    let backend_kind = match pool.backend_kind() {
        ResourcePoolBackendKind::Integrated => "Integrated",
        ResourcePoolBackendKind::Grpc => "gRPC",
        ResourcePoolBackendKind::Unspecified => "Unknown",
    };
    let backend = pool
        .backend_name
        .as_ref()
        .map(|name| format!("{backend_kind} ({name})"))
        .unwrap_or_else(|| backend_kind.to_string());
    let counts = pool
        .stats
        .as_ref()
        .filter(|_| pool.stats_status() == ResourcePoolStatsStatus::Known)
        .and_then(|stats| {
            stats
                .used
                .checked_add(stats.free)
                .map(|total| (stats.used, total))
        });
    let status = match (pool.stats_status(), counts) {
        (ResourcePoolStatsStatus::Known, Some(_)) => "Known",
        (ResourcePoolStatsStatus::Unknown, _) => "Unknown",
        _ => "Unavailable",
    };
    let (total, used) = counts
        .map(|(used, total)| (total.to_string(), usage(used, total)))
        .unwrap_or_else(|| (status.to_string(), status.to_string()));
    if let Some(message) = &pool.stats_message {
        eprintln!("{}: {message}", pool.name);
    }
    row![
        pool.name,
        backend,
        pool.remote_pool,
        pool.min.unwrap_or_default(),
        pool.max.unwrap_or_default(),
        total,
        used,
        status,
    ]
}

/// `usage` omits the percentage when an empty pool has no denominator.
fn usage(used: u64, total: u64) -> String {
    if total == 0 {
        used.to_string()
    } else {
        format!("{used} ({:.0}%)", used as f64 / total as f64 * 100.0)
    }
}
