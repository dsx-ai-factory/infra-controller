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
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;

use opentelemetry::metrics::Meter;

pub(crate) struct DpuNicFirmwareUpdateMetrics {
    pub(crate) pending_firmware_updates: Arc<AtomicU64>,
    pub(crate) unavailable_dpu_updates: Arc<AtomicU64>,
    pub(crate) running_dpu_updates: Arc<AtomicU64>,
}

impl Default for DpuNicFirmwareUpdateMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl DpuNicFirmwareUpdateMetrics {
    pub(crate) fn new() -> Self {
        DpuNicFirmwareUpdateMetrics {
            pending_firmware_updates: Arc::new(AtomicU64::new(0)),
            unavailable_dpu_updates: Arc::new(AtomicU64::new(0)),
            running_dpu_updates: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn register_callbacks(&mut self, meter: &Meter) {
        let pending_firmware_updates = self.pending_firmware_updates.clone();
        let unavailable_dpu_updates = self.unavailable_dpu_updates.clone();
        let running_dpu_updates = self.running_dpu_updates.clone();
        meter
            .u64_observable_gauge("carbide_pending_dpu_nic_firmware_update_count")
            .with_description(
                "Number of machines in the system that need a DPU/NIC firmware update",
            )
            .with_callback(move |observer| {
                observer.observe(pending_firmware_updates.load(Relaxed), &[]);
            })
            .build();

        meter
            .u64_observable_gauge("carbide_unavailable_dpu_nic_firmware_update_count")
            .with_description(
                "Number of machines in the system that need a DPU/NIC firmware update but are unavailable for update",
            )
            .with_callback(move |observer| {
                observer.observe(unavailable_dpu_updates.load(Relaxed), &[]);
            })
            .build();

        meter
            .u64_observable_gauge("carbide_running_dpu_updates_count")
            .with_description(
                "Number of machines in the system that are currently running a DPU/NIC firmware update",
            )
            .with_callback(move |observer| {
                observer.observe(running_dpu_updates.load(Relaxed), &[]);
            })
            .build();
    }
}
