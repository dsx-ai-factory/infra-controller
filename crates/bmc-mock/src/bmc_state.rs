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
use crate::redfish::account_service::AccountServiceState;
use crate::redfish::chassis::ChassisState;
use crate::redfish::computer_system::SystemState;
use crate::redfish::log_service::LogEntryDraft;
use crate::redfish::manager::ManagerState;
use crate::redfish::session_service::SessionServiceState;
use crate::redfish::update_service::UpdateServiceState;
use crate::{Callbacks, redfish};

/// BMC state and its concrete backend callbacks.
pub struct BmcState<C: Callbacks> {
    pub(crate) bmc_vendor: redfish::oem::BmcVendor,
    pub(crate) bmc_product: Option<&'static str>,
    pub(crate) bmc_redfish_version: &'static str,
    pub(crate) oem_state: redfish::oem::State,
    pub manager: Arc<ManagerState>,
    pub system_state: Arc<SystemState<C>>,
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
    pub(crate) callbacks: Option<Arc<C>>,
    /// Whether this BMC advertises and serves the `/redfish/v1/Systems`
    /// collection. Delta power shelves expose no `ComputerSystem` collection,
    /// so the service root omits the `Systems` link and the collection endpoint
    /// returns 404.
    pub(crate) exposes_computer_systems: bool,
    /// What this BMC says it can attest, or `None` for one that advertises no
    /// `ComponentIntegrity` collection: the service root omits the link and
    /// both the collection and its members return 404.
    pub(crate) component_integrities: Option<Vec<redfish::component_integrity::ComponentIntegrity>>,
}

impl<C: Callbacks> Clone for BmcState<C> {
    fn clone(&self) -> Self {
        Self {
            bmc_vendor: self.bmc_vendor,
            bmc_product: self.bmc_product,
            bmc_redfish_version: self.bmc_redfish_version,
            oem_state: self.oem_state.clone(),
            manager: self.manager.clone(),
            system_state: self.system_state.clone(),
            chassis_state: self.chassis_state.clone(),
            update_service_state: self.update_service_state.clone(),
            account_service_state: self.account_service_state.clone(),
            session_service_state: self.session_service_state.clone(),
            injection: self.injection.clone(),
            event_service: self.event_service.clone(),
            availability: self.availability.clone(),
            event_sequence: self.event_sequence.clone(),
            callbacks: self.callbacks.clone(),
            exposes_computer_systems: self.exposes_computer_systems,
            component_integrities: self.component_integrities.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum BmcEvent {
    PowerOn,
    BootCompleted,
}

impl<C: Callbacks> BmcState<C> {
    /// Number of factory resets applied to this BMC, including restored snapshots.
    pub fn factory_reset_generation(&self) -> u64 {
        self.account_service_state.reset_generation()
    }

    pub(crate) async fn factory_reset_update(&self) -> crate::BmcResetUpdate {
        let operation = self.account_service_state.lock_updates().await;
        let state = self.clone();
        crate::BmcResetUpdate {
            apply: Box::pin(async move {
                let _operation = operation;
                state
                    .account_service_state
                    .reset_to_factory()
                    .await
                    .map_err(|error| crate::ActionError::Internal(eyre::eyre!(error)))?;
                state.manager.reset_to_defaults();
                match &state.oem_state {
                    redfish::oem::State::DellIdrac(idrac) => idrac.reset_attributes(),
                    redfish::oem::State::Supermicro(supermicro) => supermicro.reset_to_defaults(),
                    _ => {}
                }
                state.session_service_state.clear();
                // A factory reset does not activate unrelated staged firmware.
                Ok(state.begin_outage())
            }),
        }
    }

    fn begin_outage(&self) -> std::time::Duration {
        let window = self
            .availability
            .as_ref()
            .map_or(std::time::Duration::ZERO, |availability| {
                availability.begin_reset()
            });
        if let Some(events) = &self.event_service {
            events.reset();
        }
        window
    }

    /// Simulate a BMC reset without changing host power: begin the outage
    /// window, if one is configured, then close event streams and clear replay
    /// history. Activates completed host BMC firmware without activating UEFI.
    /// Returns the outage duration, zero when downtime is disabled.
    pub(crate) fn reset(&self) -> std::time::Duration {
        self.update_service_state.apply_staged_bmc_firmware();
        self.begin_outage()
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
        let payload = redfish::event::response(
            &redfish::event::resource(sequence),
            &redfish::event::EventRecord {
                message_id: draft.message_id,
                message: &draft.message,
                severity: draft.severity.into(),
                timestamp: &created,
                origin: entry.as_deref().unwrap_or(&draft.origin),
            },
        );
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
                self.update_service_state.on_dpu_power_on();
                if let Some(system) = system {
                    self.record_event(LogEntryDraft::powered_on(&system));
                }
            }
            BmcEvent::BootCompleted => {
                self.on_boot_completed(true);
            }
        }
    }

    /// A libvirt warm reset keeps the live boot configuration, so it must not
    /// consume a one-shot override staged for the next cold start.
    pub(crate) fn on_boot_completed(&self, used_saved_boot_configuration: bool) {
        if used_saved_boot_configuration {
            self.system_state.on_boot_completed();
        }
        self.update_service_state.on_dpu_boot_completed();
        if let Some(system) = self.system_state.primary_system_odata_id() {
            self.record_event(LogEntryDraft::boot_completed(&system));
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
