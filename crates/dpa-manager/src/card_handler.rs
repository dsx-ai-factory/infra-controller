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
use model::dpa_interface::DpaInterfaceType;
use model::machine::ManagedHostStateSnapshot;

use crate::errors::DpaManagerResult;
use crate::metrics::DpaMonitorMetrics;
use crate::{DpaMonitor, HandlerResult};

mod astra;
mod svpc;

/// Per-interface-type state machine handlers for DPA interfaces.
#[async_trait]
pub(super) trait DpaInterfaceStateHandler: Sync {
    async fn handle_provisioning(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult>;

    async fn handle_ready(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult>;

    async fn handle_unlocking(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult>;

    async fn handle_apply_firmware(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult>;

    async fn handle_apply_profile(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult>;

    async fn handle_locking(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult>;

    async fn handle_assigned(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult>;

    /// Tenant-free NIC lockdown IKM rekey: unlock observed. Only SuperNICs
    /// participate; the default is a no-op so other interface types (Astra) that
    /// never enter the rekey states stay inert.
    async fn handle_rotate_key_unlocking(
        &self,
        _monitor: &mut DpaMonitor,
        _mh: &ManagedHostStateSnapshot,
        _idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }

    /// Tenant-free NIC lockdown IKM rekey: relock observed. Only SuperNICs
    /// participate; the default is a no-op.
    async fn handle_rotate_key_locking(
        &self,
        _monitor: &mut DpaMonitor,
        _mh: &ManagedHostStateSnapshot,
        _idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }
}

pub(super) fn handler_for(
    interface_type: DpaInterfaceType,
) -> &'static dyn DpaInterfaceStateHandler {
    match interface_type {
        DpaInterfaceType::Svpc => &svpc::SvpcInterfaceHandler,
        DpaInterfaceType::Astra => &astra::AstraInterfaceHandler,
    }
}
