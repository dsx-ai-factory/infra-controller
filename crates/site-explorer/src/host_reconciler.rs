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
//! Reconciles incomplete desired boot interfaces and initializing BF4 host links independently of creation.

use carbide_instrument::emit;
use db::Transaction;
use sqlx::PgPool;

use crate::boot_interfaces::reconcile_desired_boot_interface;
use crate::config::SiteExplorerConfig;
use crate::errors::{SiteExplorerError, SiteExplorerResult};
use crate::inventory_creator::SiteExplorerHostAdminAddressReconcileFailed;

const DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE: i64 = 100;

/// Reconciles persisted state of existing hosts.
pub(super) struct HostReconciler {
    database_connection: PgPool,
    config: SiteExplorerConfig,
}

impl HostReconciler {
    /// Stores the database pool and SiteExplorerConfig so background repair uses the configured host-PF policy.
    pub(super) fn new(database_connection: PgPool, config: SiteExplorerConfig) -> Self {
        Self {
            database_connection,
            config,
        }
    }

    /// Best-effort reconciles every host whose desired boot interface is still
    /// incomplete. Registered BF4 inventory also repairs completed provisional targets on initializing hosts.
    ///
    /// This runs independently of `create_machines`: operators may disable
    /// creation, and a host may no longer have an `ExpectedMachine` or a fresh
    /// observation. Creation-time calls remain in place so a new machine and
    /// its initial desired target still commit atomically. Keyset pages prevent
    /// permanently MAC-only hosts from starving later IDs, while one
    /// transaction per machine bounds locks and preserves completed progress.
    pub(super) async fn run_single_iteration(&self) -> SiteExplorerResult<()> {
        // Count each registered repair attempt because a host may appear in both scans.
        let record_registered_reconcile_failure = |host_machine_id, error: &db::DatabaseError| {
            emit(SiteExplorerHostAdminAddressReconcileFailed {
                host_machine_id,
                error: error.to_string(),
            });
        };
        // Revisit all initializing hosts independently of exploration rotation and completed desired pairs.
        let mut after = None;
        loop {
            let ids = db::host_boot_interface::find_initializing_host_ids(
                &self.database_connection,
                after.as_ref(),
                DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE,
            )
            .await?;
            let Some(last) = ids.last() else {
                break;
            };
            after = Some(*last);
            for id in ids {
                let _admission = db::machine_interface::admin_lock_admission().await;
                let mut txn = Transaction::begin(&self.database_connection).await?;
                let result = db::host_boot_interface::reconcile_pre_host_init(
                    txn.as_pgconn(),
                    &id,
                    self.config.dpu_host_pf_pci_controller_id,
                    self.config.admin_fnn_enabled,
                    self.config.retained_boot_interface_window,
                )
                .await;
                match result {
                    Ok(bf4) => txn.commit().await.inspect_err(|error| {
                        // A legacy host only completed the BF4 applicability check in this scan.
                        if bf4 {
                            record_registered_reconcile_failure(id, error);
                        }
                    })?,
                    Err(error) => {
                        txn.rollback_or_log("registered host-PF reconciliation failure")
                            .await;
                        record_registered_reconcile_failure(id, &error);
                    }
                }
            }
        }
        let mut after_id = None;
        loop {
            let machine_ids = db::machine_desired_boot_interface::find_incomplete_machine_ids(
                &self.database_connection,
                after_id.as_ref(),
                DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE,
            )
            .await?;
            let Some(last_id) = machine_ids.last() else {
                break;
            };
            after_id = Some(*last_id);

            for machine_id in machine_ids {
                let _admission = db::machine_interface::admin_lock_admission().await;
                let mut txn = Transaction::begin(&self.database_connection).await?;
                // Completed and incomplete BF4 targets obey the same allocation guard.
                let reconcile = match db::host_boot_interface::reconcile_pre_host_init(
                    txn.as_pgconn(),
                    &machine_id,
                    self.config.dpu_host_pf_pci_controller_id,
                    self.config.admin_fnn_enabled,
                    self.config.retained_boot_interface_window,
                )
                .await
                .inspect_err(|error| {
                    record_registered_reconcile_failure(machine_id, error);
                }) {
                    // Use ordinary enrichment only when registered BF4 policy does not apply.
                    Ok(false) => {
                        reconcile_desired_boot_interface(txn.as_pgconn(), &machine_id, None, None)
                            .await
                            .map(|()| false)
                    }
                    result => result.map_err(SiteExplorerError::from),
                };
                match reconcile {
                    Ok(bf4) => {
                        if let Err(error) = txn.commit().await {
                            // Preserve legacy desired-target diagnostics while counting a failed BF4 repair commit.
                            if bf4 {
                                record_registered_reconcile_failure(machine_id, &error);
                            } else {
                                tracing::warn!(
                                    %error,
                                    %machine_id,
                                    "Desired boot-interface reconciliation commit failed; a later Site Explorer run will retry"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        txn.rollback_or_log(
                            "site-explorer desired boot-interface reconciliation failure",
                        )
                        .await;
                        tracing::warn!(
                            %error,
                            %machine_id,
                            "Desired boot-interface reconciliation failed; a later Site Explorer run will retry"
                        );
                    }
                }
            }
        }

        Ok(())
    }
}
