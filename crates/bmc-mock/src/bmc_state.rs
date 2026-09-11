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
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::SecondsFormat;

use crate::injection::InjectionStore;
use crate::redfish;
use crate::redfish::account_service::AccountServiceState;
use crate::redfish::chassis::ChassisState;
use crate::redfish::computer_system::SystemState;
use crate::redfish::log_service::LogEntryDraft;
use crate::redfish::manager::ManagerState;
use crate::redfish::session_service::SessionServiceState;
use crate::redfish::update_service::UpdateServiceState;

#[derive(Clone)]
pub struct BmcState {
    pub(crate) bmc_vendor: redfish::oem::BmcVendor,
    pub(crate) bmc_product: Option<&'static str>,
    pub(crate) bmc_redfish_version: &'static str,
    pub(crate) oem_state: redfish::oem::State,
    pub manager: Arc<ManagerState>,
    pub system_state: Arc<SystemState>,
    pub(crate) chassis_state: Arc<ChassisState>,
    pub update_service_state: Arc<UpdateServiceState>,
    pub account_service_state: Arc<AccountServiceState>,
    pub(crate) session_service_state: Arc<SessionServiceState>,
    pub injection: Arc<InjectionStore>,
    /// Enabled SSE event service for this BMC, or None when unsupported.
    pub event_service: Option<Arc<crate::EventServiceState>>,
    /// Optional BMC outage simulation. None leaves requests available during reset.
    pub availability: Option<Arc<crate::availability::BmcAvailabilityState>>,
    /// Sequence of lifecycle Events this BMC has published.
    pub(crate) event_sequence: Arc<AtomicU64>,
    pub(crate) callbacks: Option<Arc<dyn crate::Callbacks>>,
    /// Whether this BMC advertises and serves the `/redfish/v1/Systems`
    /// collection. Delta power shelves expose no `ComputerSystem` collection,
    /// so the service root omits the `Systems` link and the collection endpoint
    /// returns 404.
    pub(crate) exposes_computer_systems: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum BmcEvent {
    PowerOn,
    BootCompleted,
}

impl BmcState {
    /// Simulate a BMC reset without changing host power: begin the outage
    /// window, if one is configured, then close event streams and clear replay
    /// history. Returns the outage duration, zero when downtime is disabled.
    pub(crate) fn reset(&self) -> std::time::Duration {
        let window = self
            .availability
            .as_ref()
            .map_or(std::time::Duration::ZERO, |a| a.begin_reset());
        if let Some(events) = &self.event_service {
            events.reset();
        }
        window
    }

    /// Record a lifecycle event the way a BMC does: append a LogEntry to the
    /// system event log when the profile has one, then publish a Redfish Event
    /// to SSE subscribers with the record inline. The event's origin is the
    /// new LogEntry when one exists, else the affected resource.
    pub(crate) fn record_event(&self, draft: LogEntryDraft) {
        let (created, entry) = self.record_log(draft.clone());
        let Some(events) = &self.event_service else {
            return;
        };
        let sequence = self.event_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let payload = redfish::event::builder(&redfish::event::resource(sequence))
            .record(&redfish::event::EventRecord {
                message_id: draft.message_id,
                message: &draft.message,
                severity: draft.severity.as_str(),
                timestamp: &created,
                origin: entry.as_deref().unwrap_or(&draft.origin),
            })
            .build();
        if let Err(error) = events.publish(payload) {
            tracing::warn!(error = %error, "lifecycle event not published");
        }
    }

    /// Append a LogEntry to the system event log when the profile has one,
    /// without publishing an Event. Returns the timestamp used and the new
    /// entry's `@odata.id`. BMC resets use this: the reset closes every stream
    /// before a subscriber could observe an announcement, as on real hardware.
    pub(crate) fn record_log(&self, draft: LogEntryDraft) -> (String, Option<String>) {
        let created = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let entry = self.system_state.record_log(draft, &created);
        (created, entry)
    }

    /// Returns whether this BMC advertises an enabled SSH serial console.
    pub fn has_enabled_ssh_serial_console(&self) -> bool {
        self.system_state.has_enabled_ssh_serial_console()
    }

    /// Overrides the client-reachable SSH serial-console port on supported systems.
    pub fn set_serial_console_ssh_port(&self, port: Option<u16>) -> bool {
        self.system_state.set_serial_console_ssh_port(port)
    }

    /// Advertises an SSH console supplied by a simulator without changing the hardware profile.
    pub fn set_simulated_serial_console_ssh_port(&self, port: Option<u16>) -> bool {
        self.system_state
            .set_simulated_serial_console_ssh_port(port)
    }

    pub fn on_event(&self, event: &BmcEvent) {
        let system = self.system_state.primary_system_odata_id();
        match event {
            BmcEvent::PowerOn => {
                self.complete_all_bios_jobs();
                self.apply_pending_bluefield_mode();
                // Move any staged firmware versions into the active inventory so
                // that site-explorer observes the upgraded version after reset.
                self.update_service_state.apply_staged_firmware();
                if let Some(system) = system {
                    self.record_event(LogEntryDraft::powered_on(&system));
                }
            }
            BmcEvent::BootCompleted => {
                self.system_state.on_boot_completed();
                if let Some(system) = system {
                    self.record_event(LogEntryDraft::boot_completed(&system));
                }
            }
        }
    }

    fn complete_all_bios_jobs(&self) {
        if let redfish::oem::State::DellIdrac(v) = &self.oem_state {
            v.complete_all_bios_jobs()
        }
    }

    /// Apply a BlueField's queued `Mode.Set` (the BF-3 OEM DPU/NIC mode flip),
    /// if any. Real hardware picks up the staged mode only after a power cycle,
    /// so this runs on `PowerOn`.
    fn apply_pending_bluefield_mode(&self) {
        if let redfish::oem::State::NvidiaBluefield(v) = &self.oem_state {
            v.apply_pending_mode();
        }
    }

    /// The BlueField's current NIC-mode flag, or `None` for a BMC that does not
    /// mock a BlueField (e.g. the host iDRAC). Lets machine-a-tron observe a DPU
    /// that flipped to NIC mode on a power cycle and converge to NIC behavior.
    pub fn bluefield_nic_mode(&self) -> Option<bool> {
        match &self.oem_state {
            redfish::oem::State::NvidiaBluefield(v) => Some(v.nic_mode()),
            _ => None,
        }
    }
}
