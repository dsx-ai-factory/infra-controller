// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Account for admitted NIC work in the shared host update budget. New NIC
//! admission remains with <https://github.com/dsx-ai-factory/infra-controller/issues/6680>.

use std::collections::{HashMap, HashSet};
use std::fmt;

use async_trait::async_trait;
use carbide_uuid::machine::HostMachineId;
use db::work_lock_manager::WorkLock;
use model::machine::ManagedHostStateSnapshot;
use sqlx::PgConnection;

use super::machine_update_module::MachineUpdateModule;
use super::metrics::FirmwareUpdateProgress;
use crate::CarbideResult;

#[cfg(test)]
mod tests;

pub(super) struct NicFirmwareUpdate;

#[async_trait]
impl MachineUpdateModule for NicFirmwareUpdate {
    async fn get_updates_in_progress(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        // Every `NicFirmwareUpdating` phase occupies a slot, including Failed.
        // This module admits no work; the slot remains until the host leaves
        // that state, regardless of receipt availability.
        Ok(db::machine::find_host_nic_firmware_operations(txn)
            .await?
            .into_iter()
            .map(|(host_id, _)| host_id)
            .collect())
    }

    async fn start_updates(
        &self,
        _pool: &sqlx::Pool<sqlx::Postgres>,
        _work_lock: &WorkLock,
        _available_updates: i32,
        _updating_host_machines: &HashSet<HostMachineId>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        Ok(HashSet::new())
    }

    async fn clear_completed_updates(
        &self,
        _txn: &mut PgConnection,
    ) -> CarbideResult<Vec<FirmwareUpdateProgress>> {
        Ok(Vec::new())
    }

    async fn update_metrics(
        &self,
        _pool: &sqlx::Pool<sqlx::Postgres>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<()> {
        Ok(())
    }
}

impl fmt::Display for NicFirmwareUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NicFirmwareUpdate")
    }
}
