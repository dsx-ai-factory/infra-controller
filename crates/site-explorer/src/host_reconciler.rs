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
//! Reconciles incomplete desired boot interfaces independently of inventory creation.

use db::Transaction;
use sqlx::PgPool;

use crate::boot_interfaces::reconcile_desired_boot_interface;
use crate::errors::SiteExplorerResult;

const DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE: i64 = 100;

/// Reconciles persisted state of existing hosts.
pub(super) struct HostReconciler {
    database_connection: PgPool,
}

impl HostReconciler {
    pub(super) fn new(database_connection: PgPool) -> Self {
        Self {
            database_connection,
        }
    }

    /// Runs one best-effort pass over hosts with incomplete desired boot interfaces.
    ///
    /// This runs independently of `create_machines`: operators may disable
    /// creation, and a host may no longer have an `ExpectedMachine` or a fresh
    /// observation. Creation-time calls remain in place so a new machine and
    /// its initial desired target still commit atomically. Keyset pages prevent
    /// permanently MAC-only hosts from starving later IDs, while one
    /// transaction per machine bounds locks and preserves completed progress.
    pub(super) async fn run_single_iteration(&self) -> SiteExplorerResult<()> {
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
                let mut txn = Transaction::begin(&self.database_connection).await?;
                match reconcile_desired_boot_interface(txn.as_pgconn(), &machine_id, None, None)
                    .await
                {
                    Ok(()) => {
                        if let Err(error) = txn.commit().await {
                            tracing::warn!(
                                %error,
                                %machine_id,
                                "Desired boot-interface reconciliation commit failed; a later Site Explorer run will retry"
                            );
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
