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

use async_trait::async_trait;
use model::dpa_interface::DpaInterfaceControllerState;
use model::machine::ManagedHostStateSnapshot;

use super::DpaInterfaceStateHandler;
use crate::errors::DpaManagerResult;
use crate::metrics::DpaMonitorMetrics;
use crate::{DpaMonitor, HandlerResult};

pub(super) struct AstraInterfaceHandler;

#[async_trait]
impl DpaInterfaceStateHandler for AstraInterfaceHandler {
    async fn handle_provisioning(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        if idx >= mh.dpa_interface_snapshots.len() {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_provisioning index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let dpa_interface = &mh.dpa_interface_snapshots[idx];

        let host_use_admin_network = dpa_interface.use_admin_network();
        if host_use_admin_network {
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let new_state = DpaInterfaceControllerState::Ready;
        tracing::info!(next_state = ?new_state, "Dpa Interface state transition");
        Ok(HandlerResult {
            new_state: Some(new_state),
            txn: None,
        })
    }

    async fn handle_ready(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        if idx >= mh.dpa_interface_snapshots.len() {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_ready index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let dpa_interface = &mh.dpa_interface_snapshots[idx];

        let host_use_admin_network = dpa_interface.use_admin_network();
        if !host_use_admin_network {
            let new_state = DpaInterfaceControllerState::Assigned;
            tracing::info!(next_state = ?new_state, "Dpa Interface state transition");

            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: None,
            });
        }

        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }

    async fn handle_unlocking(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        if idx >= mh.dpa_interface_snapshots.len() {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_unlocking index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let dpa_interface = &mh.dpa_interface_snapshots[idx];

        tracing::warn!(
            dpa_interface_id = %dpa_interface.id,
            controller_state = ?dpa_interface.controller_state.value,
            "Astra DPA interface is in an unexpected state",
        );

        return Ok(HandlerResult {
            new_state: None,
            txn: None,
        });
    }

    async fn handle_apply_firmware(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        if idx >= mh.dpa_interface_snapshots.len() {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_apply_firmware index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let dpa_interface = &mh.dpa_interface_snapshots[idx];

        tracing::warn!(
            dpa_interface_id = %dpa_interface.id,
            controller_state = ?dpa_interface.controller_state.value,
            "Astra DPA interface is in an unexpected state",
        );

        return Ok(HandlerResult {
            new_state: None,
            txn: None,
        });
    }

    async fn handle_apply_profile(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        if idx >= mh.dpa_interface_snapshots.len() {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_apply_profile index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let dpa_interface = &mh.dpa_interface_snapshots[idx];
        tracing::warn!(
            dpa_interface_id = %dpa_interface.id,
            controller_state = ?dpa_interface.controller_state.value,
            "Astra DPA interface is in an unexpected state",
        );
        return Ok(HandlerResult {
            new_state: None,
            txn: None,
        });
    }

    async fn handle_locking(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        if idx >= mh.dpa_interface_snapshots.len() {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_locking index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let dpa_interface = &mh.dpa_interface_snapshots[idx];
        tracing::warn!(
            dpa_interface_id = %dpa_interface.id,
            controller_state = ?dpa_interface.controller_state.value,
            "Astra DPA interface is in an unexpected state",
        );
        return Ok(HandlerResult {
            new_state: None,
            txn: None,
        });
    }

    async fn handle_assigned(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        if idx >= mh.dpa_interface_snapshots.len() {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_assigned index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let dpa_interface = &mh.dpa_interface_snapshots[idx];

        let host_use_admin_network = dpa_interface.use_admin_network();

        if host_use_admin_network {
            let new_state = DpaInterfaceControllerState::Ready;
            tracing::info!(next_state = ?new_state, "Dpa Interface state transition");
            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: None,
            });
        }

        return Ok(HandlerResult {
            new_state: None,
            txn: None,
        });
    }
}
