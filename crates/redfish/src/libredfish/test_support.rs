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

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use carbide_secrets::credentials::{CredentialReader, Credentials};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use chrono::Utc;
use libredfish::model::certificate::Certificate;
use libredfish::model::component_integrity::{ComponentIntegrities, ComponentIntegrity};
use libredfish::model::oem::nvidia_dpu::{HostPrivilegeLevel, NicMode};
use libredfish::model::secure_boot::SecureBootMode;
use libredfish::model::sensor::GPUSensors;
use libredfish::model::service_root::{RedfishVendor, ServiceRoot};
use libredfish::model::software_inventory::SoftwareInventory;
use libredfish::model::storage::Drives;
use libredfish::model::task::Task;
use libredfish::model::update_service::{ComponentType, TransferProtocolType, UpdateService};
use libredfish::model::{ODataId, ODataLinks};
use libredfish::standard::RedfishStandard;
use libredfish::{
    Assembly, Chassis, Collection, EnabledDisabled, JobState, ManagerResetType, NetworkAdapter,
    PowerState, Redfish, RedfishError, Resource, SpxNicModelAndName, SystemPowerControl,
};
use mac_address::MacAddress;

use crate::libredfish::{RedfishAuth, RedfishClientCreationError, RedfishClientPool};

const TRIGGER_EVIDENCE_TASK_ID: &str = "SpdmTriggerEvidenceTaskId";

#[derive(Default)]
struct RedfishSimState {
    hosts: HashMap<String, RedfishSimHostState>,
    users: HashMap<String, String>,
    fw_version: Arc<String>,
    secure_boot: AtomicBool,
    no_component_integrities: bool,
    firmware_for_component_error: bool,
    get_task_trigger_evidence_returns_interrupted: bool,
    machine_setup_bios_job_id: Option<String>,
    is_bios_setup: Option<bool>,
    default_lockdown: Option<EnabledDisabled>,
    boss_controller_id: Option<String>,
    /// Override whether `lockdown_bmc` changes the observed state. `None`
    /// preserves the normal successful behavior; `Some(false)` models a BMC
    /// accepting the write without applying the requested policy.
    lockdown_bmc_applies: Option<bool>,
    job_state_sequence: VecDeque<JobState>,
    /// Offset (in seconds) applied to the BMC `DateTime` returned by
    /// `get_manager`, relative to the controller's `Utc::now()`. Defaults to 0
    /// (perfectly in sync); tests set it to simulate a BMC clock that is out of
    /// sync to exercise the time-sync reset/retry path.
    bmc_time_offset_seconds: i64,
    /// Records every call to `RedfishClientPool::create_client` so tests can
    /// assert what vendor was passed at each call site.
    create_client_calls: Vec<CreateClientCall>,
    /// When set, `change_password` fails with
    /// [`RedfishError::PasswordChangeRequired`] to model a factory BMC (e.g.
    /// Viking) that refuses the by-username change until the initial
    /// change-on-first-use has been done -- the case the `AMI`/`LenovoGB300`
    /// rotation path handles by retrying `change_password_by_id("2")`.
    password_change_required: bool,
    /// Optional account URI returned with [`RedfishError::PasswordChangeRequired`].
    password_change_required_account_uri: Option<String>,
    /// When set, overrides the `Vendor` field returned by `get_service_root`.
    /// Tests set it to an unrecognized value to force `probe_bmc_vendor` down
    /// the Chassis `Manufacturer` fallback path.
    service_root_vendor: Option<String>,
    /// When set, overrides the `Product` field returned by `get_service_root`.
    /// Tests use this to model a specific DPU generation.
    service_root_product: Option<String>,
    /// When set, overrides the `Manufacturer` returned by `get_chassis`, so
    /// tests can drive `probe_bmc_vendor`'s Lite-On/Delta chassis fallback.
    chassis_manufacturer: Option<String>,
    platform_actions: Vec<RedfishSimPlatformAction>,
    /// Lossless `machine_setup_status` target observations. These reads stay
    /// separate from mutating platform actions and preserve both `Pair` fields.
    machine_setup_status_targets: HashMap<String, Vec<Option<RedfishSimBootInterfaceRef>>>,
    /// Opt-in authentication enforcement. Off by default so existing tests
    /// (which pass arbitrary or anonymous credentials) are undisturbed. When on,
    /// `get_accounts` returns `401` unless the client was created with a
    /// `Direct` credential whose password matches the seeded `users` entry, so
    /// credential-probe paths (e.g. `bmc_credentials_valid`) can be exercised.
    enforce_auth: bool,
    auth_attempts: Vec<RedfishSimAuthAttempt>,
    /// When set, `get_accounts` fails with a non-authentication transport error
    /// (`503`), so callers' error-propagation paths can be exercised distinctly
    /// from an unauthorized rejection.
    get_accounts_error: bool,
    /// When set, BMC event-log reads succeed with an empty log.
    bmc_event_log_supported: bool,
    /// When set, the next BMC event-log read fails with a transient error.
    bmc_event_log_error_once: bool,
    /// Opt-in password-reuse policy. When on, a password *change* whose new
    /// value equals the account's current password is rejected (`400`), modeling
    /// the real BMCs that refuse a same-value change -- the exact behavior BMC
    /// credential rotation's crash recovery must avoid triggering.
    reject_password_reuse: bool,
    /// When set, every password *change* fails with a
    /// [`RedfishError::GenericError`] carrying this message (tests seed it with a
    /// secret to assert redaction end to end). Takes precedence over the auth
    /// and reuse checks so it can model a change that fails after authenticating.
    change_password_error: Option<String>,
    /// When set, every `change_uefi_password` fails with a
    /// [`RedfishError::GenericError`] carrying this message, modeling a BIOS that
    /// rejects the UEFI password change (e.g. every current-password candidate is
    /// wrong). Tests seed it with a secret to assert the recorded rotation error
    /// is password-redacted, and to exercise the quarantine-and-return-to-Ready
    /// path in host UEFI rotation.
    uefi_password_change_error: Option<String>,
    /// When set, every `RedfishClientPool::create_client` fails with a
    /// [`RedfishError::GenericError`] carrying this message, modeling a
    /// transient client-creation failure (credential store, TCP, or the
    /// vendor probe). Drives the [`super::CredentialOpError::ClientCreation`]
    /// retry paths at credential-op call sites.
    create_client_error: Option<String>,
    /// When set, the sim's `BmcCredentialOps::uefi_setup` override fails with
    /// [`super::CredentialOpError::ClientCreation`] carrying this message,
    /// modeling a client-creation failure inside the credential op (the sim
    /// override replaces the default body, so `create_client_error` cannot
    /// reach it).
    uefi_setup_client_creation_error: Option<String>,
    /// BIOS attribute map returned by `bios()` when set; the sim's `bios()`
    /// is otherwise unimplemented. Lets unit tests drive the default
    /// `BmcCredentialOps::uefi_setup` DPU body past its attribute probe.
    bios_attributes: Option<HashMap<String, serde_json::Value>>,
    /// Optional ComputerSystem identifier used to drive platform classification.
    system_id: Option<String>,
    /// Physical-port MAC addresses exposed through the adapter Ports collection.
    network_adapter_port_mac_addresses: Vec<MacAddress>,
    /// Chassis linked from the simulated ComputerSystem.
    system_chassis_ids: Vec<String>,
}

/// Build the `HTTPErrorCode` a real BMC would return for a rejected request, so
/// [`RedfishError::is_unauthorized`] (and callers keying off the status) behave
/// as they do against hardware.
fn sim_http_error(status: http::StatusCode, url: &str, body: &str) -> RedfishError {
    RedfishError::HTTPErrorCode {
        url: url.to_string(),
        status_code: status,
        response_body: body.to_string(),
    }
}

/// Snapshot of a single `RedfishClientPool::create_client` invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateClientCall {
    pub host: String,
    pub vendor: Option<RedfishVendor>,
}

/// Credential and result observed when the simulator checks direct authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedfishSimAuthAttempt {
    pub credentials: Credentials,
    pub authorized: bool,
}

#[derive(Debug)]
struct RedfishSimHostState {
    power: PowerState,
    lockdown: libredfish::EnabledDisabled,
    actions: Vec<RedfishSimAction>,
    boot_interface_targets: Vec<Option<RedfishSimBootInterfaceRef>>,
    /// Whether this host's `HttpDev1` UEFI HTTP-boot device is enabled in BIOS.
    /// Defaults to `true` (the steady state after `machine_setup`): the boot
    /// device is present, so `set_boot_order_dpu_first` can promote it and
    /// `is_boot_order_setup` reports the order as configured. Tests flip it to
    /// `false` via [`RedfishSim::set_http_dev1_reverted`] to model a NIC-mode
    /// reboot de-enumerating the BlueField, which reverts the attribute to the
    /// onboard default; only a fresh `machine_setup` re-enables it. Per-host so
    /// one host's de-enumeration (or recovery) doesn't bleed into the others.
    http_dev1_enabled: bool,
    /// Whether this host reports its boot order as configured. `None` defers to
    /// the default (`true`); `set_boot_order_dpu_first` records it as
    /// `Some(http_dev1_enabled)` -- the reorder only "sticks" while the HTTP
    /// boot device is present -- and `set_is_boot_order_setup` forces it.
    /// Per-host so one host's boot-order state can't flip another host's
    /// `is_boot_order_setup` check.
    is_boot_order_setup: Option<bool>,
}

impl Default for RedfishSimHostState {
    fn default() -> Self {
        Self {
            power: PowerState::default(),
            lockdown: libredfish::EnabledDisabled::Disabled,
            actions: Vec::default(),
            boot_interface_targets: Vec::default(),
            // Enabled by default so existing tests, which never model a
            // de-enumeration, see the boot order configure normally.
            http_dev1_enabled: true,
            is_boot_order_setup: None,
        }
    }
}

#[derive(Default)]
pub struct RedfishSim {
    state: Arc<Mutex<RedfishSimState>>,
    credential_manager: TestCredentialManager,
}

impl RedfishSim {
    pub fn timepoint(&self) -> RedfishSimTimepoint {
        RedfishSimTimepoint {
            pos: self
                .state
                .lock()
                .unwrap()
                .hosts
                .iter()
                .map(|(host, state)| (host.clone(), state.actions.len()))
                .collect(),
        }
    }

    pub fn actions_since(&self, timepoint: &RedfishSimTimepoint) -> RedfishSimActions {
        let state = self.state.lock().unwrap();
        RedfishSimActions {
            host_actions: state
                .hosts
                .iter()
                .map(|(host, state)| {
                    (
                        host.clone(),
                        timepoint
                            .pos
                            .get(host)
                            .map(|pos| state.actions[*pos..].to_vec())
                            .unwrap_or_else(|| state.actions.clone()),
                    )
                })
                .collect(),
        }
    }

    /// Return every logical boot-interface selector supplied to
    /// `machine_setup`, `is_bios_setup`, `is_boot_order_setup`, or
    /// `set_boot_order_dpu_first` on one endpoint.
    pub fn boot_interface_targets(&self, host: &str) -> Vec<Option<RedfishSimBootInterfaceRef>> {
        self.state
            .lock()
            .unwrap()
            .hosts
            .get(host)
            .map(|state| state.boot_interface_targets.clone())
            .unwrap_or_default()
    }

    /// Return the simulated lockdown state for each Redfish client target.
    pub fn lockdown_states(&self) -> Vec<EnabledDisabled> {
        self.state
            .lock()
            .unwrap()
            .hosts
            .values()
            .map(|host| host.lockdown)
            .collect()
    }

    /// Return calls related to platform configuration and UEFI credentials.
    pub fn platform_actions(&self) -> Vec<RedfishSimPlatformAction> {
        self.state.lock().unwrap().platform_actions.clone()
    }

    /// Returns each boot-interface selector supplied to
    /// `Redfish::machine_setup_status` for one simulated endpoint.
    pub fn machine_setup_status_targets(
        &self,
        host: &str,
    ) -> Vec<Option<RedfishSimBootInterfaceRef>> {
        self.state
            .lock()
            .unwrap()
            .machine_setup_status_targets
            .get(host)
            .cloned()
            .unwrap_or_default()
    }

    /// Model a BMC that exposes no `ComponentIntegrity` collection, so nothing
    /// is eligible for SPDM attestation.
    pub fn set_no_component_integrities(&self, no_component_integrities: bool) {
        self.state.lock().unwrap().no_component_integrities = no_component_integrities;
    }

    /// Fail the firmware-inventory lookup an attestation makes while fetching
    /// component metadata.
    pub fn set_firmware_for_component_error(&self, error: bool) {
        self.state.lock().unwrap().firmware_for_component_error = error;
    }

    /// Return the evidence-collection task as `Interrupted`, modelling a BMC
    /// that keeps failing to produce evidence.
    pub fn set_get_task_trigger_evidence_returns_interrupted(&self, interrupted: bool) {
        self.state
            .lock()
            .unwrap()
            .get_task_trigger_evidence_returns_interrupted = interrupted;
    }

    pub fn set_machine_setup_bios_job_id(&self, job_id: Option<String>) {
        self.state.lock().unwrap().machine_setup_bios_job_id = job_id;
    }

    pub fn set_job_state_sequence(&self, states: Vec<JobState>) {
        self.state.lock().unwrap().job_state_sequence = VecDeque::from(states);
    }

    pub fn set_boss_controller_id(&self, boss_controller_id: Option<String>) {
        self.state.lock().unwrap().boss_controller_id = boss_controller_id;
    }

    pub fn set_is_bios_setup(&self, ready: bool) {
        self.state.lock().unwrap().is_bios_setup = Some(ready);
    }

    /// Force whether simulated Redfish reports the boot order as configured,
    /// for every current host. A later `set_boot_order_dpu_first` overwrites it
    /// per host (last write wins), the same as a real boot-order setup would.
    pub fn set_is_boot_order_setup(&self, ready: bool) {
        let mut state = self.state.lock().unwrap();
        for host_state in state.hosts.values_mut() {
            host_state.is_boot_order_setup = Some(ready);
        }
    }

    /// Model a NIC-mode reboot de-enumerating the BlueField: the `HttpDev1`
    /// UEFI HTTP-boot device reverts to the onboard default and is no longer
    /// enabled. While reverted, `set_boot_order_dpu_first` records the boot
    /// order as *not* configured (it can only reorder a device that exists),
    /// so `is_boot_order_setup` reports `false` until a fresh `machine_setup`
    /// re-enables the device.
    ///
    /// The flag is per-host, so reverting every current host here is just the
    /// no-host-id entry point; a later `machine_setup` re-enables only the host
    /// it targets, which is what keeps multi-host tests isolated.
    pub fn set_http_dev1_reverted(&self) {
        let mut state = self.state.lock().unwrap();
        for host_state in state.hosts.values_mut() {
            host_state.http_dev1_enabled = false;
        }
    }

    /// Configure simulated BMC lockdown state for existing and future clients.
    pub fn set_lockdown(&self, lockdown: EnabledDisabled) {
        let mut state = self.state.lock().unwrap();
        state.default_lockdown = Some(lockdown);
        for host_state in state.hosts.values_mut() {
            host_state.lockdown = lockdown;
        }
    }

    /// Control whether `lockdown_bmc` updates the observed lockdown state.
    pub fn set_lockdown_bmc_applies(&self, applies: bool) {
        self.state.lock().unwrap().lockdown_bmc_applies = Some(applies);
    }

    /// Set the offset (in seconds) applied to the BMC `DateTime` returned by
    /// `get_manager`, relative to the controller clock. Use a value larger than
    /// the time-sync threshold to simulate an out-of-sync BMC clock.
    pub fn set_bmc_time_offset_seconds(&self, offset: i64) {
        self.state.lock().unwrap().bmc_time_offset_seconds = offset;
    }

    /// Returns a snapshot of every `create_client` call made through this sim,
    /// in the order they happened. Useful for asserting which vendor was
    /// passed at a given call site.
    pub fn create_client_calls(&self) -> Vec<CreateClientCall> {
        self.state.lock().unwrap().create_client_calls.clone()
    }

    /// Return direct authentication checks in the order the simulator performed them.
    pub fn auth_attempts(&self) -> Vec<RedfishSimAuthAttempt> {
        self.state.lock().unwrap().auth_attempts.clone()
    }

    /// Seed a user account so calls like `change_password` /
    /// `change_password_by_id` see it as already present.
    pub fn seed_user(&self, username: &str, password: &str) {
        self.state
            .lock()
            .unwrap()
            .users
            .insert(username.to_string(), password.to_string());
    }

    pub fn user_password(&self, account_id: &str) -> Option<String> {
        self.state.lock().unwrap().users.get(account_id).cloned()
    }

    /// Make `change_password` (the by-username path) fail with
    /// [`RedfishError::PasswordChangeRequired`], modeling a factory BMC that
    /// blocks it until change-on-first-use. `change_password_by_id` still
    /// succeeds, so this exercises the `AMI`/`LenovoGB300` rotation fallback.
    pub fn set_password_change_required(&self, required: bool, account_uri: Option<&str>) {
        let mut state = self.state.lock().unwrap();
        state.password_change_required = required;
        state.password_change_required_account_uri = if required {
            account_uri.map(str::to_string)
        } else {
            None
        };
    }

    /// Enable opt-in authentication enforcement (see [`RedfishSimState::enforce_auth`]):
    /// once on, `get_accounts` authorizes against the seeded `users` map, so
    /// credential-probe paths can distinguish valid from rejected credentials.
    pub fn set_enforce_auth(&self, enforce: bool) {
        self.state.lock().unwrap().enforce_auth = enforce;
    }

    /// Force the next `get_accounts` calls to fail with a non-authentication
    /// transport error (`503`), to exercise a caller's error-propagation path.
    pub fn set_get_accounts_error(&self, error: bool) {
        self.state.lock().unwrap().get_accounts_error = error;
    }

    /// Control whether BMC event-log reads succeed with an empty log.
    pub fn set_bmc_event_log_supported(&self, supported: bool) {
        self.state.lock().unwrap().bmc_event_log_supported = supported;
    }

    /// Fail the next BMC event-log read with a transient simulated error.
    pub fn fail_next_bmc_event_log_read(&self) {
        self.state.lock().unwrap().bmc_event_log_error_once = true;
    }

    /// Enable the opt-in password-reuse policy (see
    /// [`RedfishSimState::reject_password_reuse`]): a same-value password change
    /// is rejected, so a caller that must not issue one is held to it.
    pub fn set_reject_password_reuse(&self, reject: bool) {
        self.state.lock().unwrap().reject_password_reuse = reject;
    }

    /// Force every password change to fail with a [`RedfishError::GenericError`]
    /// carrying `message`, so redaction of the recorded error can be asserted.
    pub fn set_change_password_error(&self, message: impl Into<String>) {
        self.state.lock().unwrap().change_password_error = Some(message.into());
    }

    /// Force every `change_uefi_password` to fail with a
    /// [`RedfishError::GenericError`] carrying `message`, modeling a BIOS that
    /// rejects the UEFI password change. Drives the host UEFI rotation
    /// quarantine-and-return-to-Ready path; seed with a secret to assert the
    /// recorded rotation error is redacted.
    pub fn set_uefi_password_change_error(&self, message: impl Into<String>) {
        self.state.lock().unwrap().uefi_password_change_error = Some(message.into());
    }

    /// Force every `create_client` to fail with a
    /// [`RedfishError::GenericError`] carrying `message`, modeling a transient
    /// client-creation failure (credential store, TCP, or the vendor probe).
    /// Clear with [`Self::clear_create_client_error`] to model recovery.
    pub fn set_create_client_error(&self, message: impl Into<String>) {
        self.state.lock().unwrap().create_client_error = Some(message.into());
    }

    /// Let `create_client` succeed again after
    /// [`Self::set_create_client_error`], modeling the transient failure
    /// clearing so a retry can be asserted.
    pub fn clear_create_client_error(&self) {
        self.state.lock().unwrap().create_client_error = None;
    }

    /// Force the sim's `uefi_setup` override to fail with
    /// [`super::CredentialOpError::ClientCreation`] carrying `message`. The
    /// override replaces the default body, so this (not
    /// [`Self::set_create_client_error`]) models a client-creation failure
    /// for callers of `uefi_setup`.
    pub fn set_uefi_setup_client_creation_error(&self, message: impl Into<String>) {
        self.state.lock().unwrap().uefi_setup_client_creation_error = Some(message.into());
    }

    /// Let `uefi_setup` succeed again after
    /// [`Self::set_uefi_setup_client_creation_error`].
    pub fn clear_uefi_setup_client_creation_error(&self) {
        self.state.lock().unwrap().uefi_setup_client_creation_error = None;
    }

    /// Set the BIOS attribute map returned by the sim client's `bios()`,
    /// which is otherwise unimplemented.
    pub fn set_bios_attributes(&self, attributes: HashMap<String, serde_json::Value>) {
        self.state.lock().unwrap().bios_attributes = Some(attributes);
    }

    /// Override the `Vendor` reported by `get_service_root`. Set it to an
    /// unrecognized value to force `probe_bmc_vendor` past the anonymous
    /// service-root probe and into the Chassis `Manufacturer` fallback.
    pub fn set_service_root_vendor(&self, vendor: Option<String>) {
        self.state.lock().unwrap().service_root_vendor = vendor;
    }

    /// Override the `Product` reported by `get_service_root`, allowing tests to
    /// drive model-specific behavior such as DPU factory credential selection.
    pub fn set_service_root_product(&self, product: Option<String>) {
        self.state.lock().unwrap().service_root_product = product;
    }

    /// Override the `Manufacturer` reported by `get_chassis`, so tests can
    /// drive `probe_bmc_vendor`'s Lite-On/Delta chassis fallback.
    pub fn set_chassis_manufacturer(&self, manufacturer: Option<String>) {
        self.state.lock().unwrap().chassis_manufacturer = manufacturer;
    }

    /// Override the ComputerSystem identifier returned by the simulator. Site
    /// Explorer classifies identifiers containing `bluefield` as DPUs.
    pub fn set_system_id(&self, system_id: impl Into<String>) {
        self.state.lock().unwrap().system_id = Some(system_id.into());
    }

    /// Configure the physical-port MAC addresses returned by the simulated
    /// `Chassis/.../NetworkAdapters/.../Ports` collection. A non-empty value
    /// also advertises the parent `NetworkAdapters` link on `Card1`.
    pub fn set_network_adapter_port_mac_addresses(&self, mac_addresses: Vec<MacAddress>) {
        self.state
            .lock()
            .unwrap()
            .network_adapter_port_mac_addresses = mac_addresses;
    }

    pub fn set_system_chassis_ids(&self, chassis_ids: Vec<String>) {
        self.state.lock().unwrap().system_chassis_ids = chassis_ids;
    }

    /// Seed a credential into the sim's credential store -- the same store
    /// [`Self::credential_reader`] exposes. Controllers that resolve a credential
    /// through `redfish_client_pool.credential_reader()` (e.g. UEFI setup, which
    /// now reads the site-wide credential in the controller before calling
    /// `uefi_setup`) read from here, so tests that drive those paths must seed it.
    pub async fn seed_credential(
        &self,
        key: &carbide_secrets::credentials::CredentialKey,
        credentials: &carbide_secrets::credentials::Credentials,
    ) {
        use carbide_secrets::credentials::CredentialWriter;
        self.credential_manager
            .set_credentials(key, credentials)
            .await
            .expect("seed redfish-sim credential");
    }
}

pub struct RedfishSimTimepoint {
    pos: HashMap<String, usize>,
}

/// Platform-configuration calls recorded separately from power actions.
#[derive(Debug, Clone, PartialEq)]
pub enum RedfishSimPlatformAction {
    SetHostRshim { host: String },
    SetHostPrivilegeLevel { host: String },
    IsBiosSetup { host: String },
    UefiSetup { dpu: bool },
}

/// Owned form of the boot-interface reference observed by the Redfish
/// simulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedfishSimBootInterfaceRef {
    Mac(MacAddress),
    InterfaceId(String),
    Pair {
        mac_address: MacAddress,
        interface_id: String,
    },
}

impl From<libredfish::BootInterfaceRef<'_>> for RedfishSimBootInterfaceRef {
    fn from(value: libredfish::BootInterfaceRef<'_>) -> Self {
        match value {
            libredfish::BootInterfaceRef::Mac(mac_address) => Self::Mac(mac_address),
            libredfish::BootInterfaceRef::InterfaceId(interface_id) => {
                Self::InterfaceId(interface_id.to_string())
            }
            libredfish::BootInterfaceRef::Pair {
                mac_address,
                interface_id,
            } => Self::Pair {
                mac_address,
                interface_id: interface_id.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RedfishSimAction {
    Power(libredfish::SystemPowerControl),
    BmcReset(Option<ManagerResetType>),
    SetUtcTimezone,
    SetNtpServers(Vec<String>),
    MachineSetup {
        oem_manager_profiles: libredfish::BiosProfileVendor,
        /// The boot interface the setup call targeted (`None` when the caller
        /// ran setup without one, e.g. DPU setup), letting tests assert which
        /// NIC boot-device configuration was applied for.
        boot_interface_mac: Option<String>,
    },
    /// Records a call to `Redfish::is_boot_order_setup`, letting
    /// tests assert that the managed-host state controller actually
    /// asked the BMC about boot order for a given MAC. Mainly used
    /// a regression check for zero-DPU hosts to make sure we're still
    /// giving them the love they deserve.
    IsBootOrderSetup {
        boot_interface_mac: String,
    },
    /// Records a call to `Redfish::set_boot_order_dpu_first`, which is
    /// used to make the given MAC the first boot device (which zero DPU
    /// hosts flow through as well using the host NIC MAC).
    SetBootOrderDpuFirst {
        boot_interface_mac: String,
    },
}

pub struct RedfishSimActions {
    host_actions: HashMap<String, Vec<RedfishSimAction>>,
}

impl RedfishSimActions {
    pub fn all_hosts(&self) -> Vec<RedfishSimAction> {
        self.host_actions
            .values()
            .flat_map(|actions| actions.iter().cloned())
            .collect()
    }

    /// Return Redfish actions issued to one simulated endpoint.
    pub fn for_host(&self, host: &str) -> Vec<RedfishSimAction> {
        self.host_actions.get(host).cloned().unwrap_or_default()
    }
}

/// Stringifies a [`libredfish::BootInterfaceRef`] for recording in
/// [`RedfishSimAction`], so tests can assert on the targeted boot interface
/// regardless of which variant was used. Paired targets use their MAC because
/// the simulator action fields and existing assertions model boot-interface
/// targets as MAC strings.
fn boot_interface_ref_to_string(boot_interface: libredfish::BootInterfaceRef<'_>) -> String {
    match boot_interface {
        libredfish::BootInterfaceRef::Mac(mac)
        | libredfish::BootInterfaceRef::Pair {
            mac_address: mac, ..
        } => mac.to_string(),
        libredfish::BootInterfaceRef::InterfaceId(id) => id.to_string(),
    }
}

struct RedfishSimClient {
    state: Arc<Mutex<RedfishSimState>>,
    _host: String,
    _port: Option<u16>,
    /// Credential this client was created with. Ignored unless
    /// [`RedfishSimState::enforce_auth`] is on, in which case authenticated
    /// operations authorize against it.
    auth: RedfishAuth,
}

impl RedfishSimClient {
    /// Under [`RedfishSimState::enforce_auth`], authorize the credential this
    /// client was created with against the seeded `users`. Returns a `401`
    /// error on a mismatch (or a non-`Direct` credential); a no-op when
    /// enforcement is off, preserving the behavior existing tests rely on.
    fn authorize(&self, state: &mut RedfishSimState, url: &str) -> Result<(), RedfishError> {
        if !state.enforce_auth {
            return Ok(());
        }
        let authorized = match &self.auth {
            RedfishAuth::Direct(username, password) => {
                let authorized = state
                    .users
                    .get(username)
                    .is_some_and(|stored| stored == password);
                state.auth_attempts.push(RedfishSimAuthAttempt {
                    credentials: Credentials::new(username.clone(), password.clone()),
                    authorized,
                });
                authorized
            }
            RedfishAuth::Anonymous | RedfishAuth::Key(_) => false,
        };
        if authorized {
            Ok(())
        } else {
            Err(sim_http_error(
                http::StatusCode::UNAUTHORIZED,
                url,
                "sim: unauthorized",
            ))
        }
    }
}

impl Redfish for RedfishSimClient {
    fn std_redfish(&self) -> &RedfishStandard {
        panic!("RedfishSimClient must implement Redfish operations directly")
    }

    fn get_power_state<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::PowerState, RedfishError>> {
        Box::pin(async move { Ok(self.state.lock().unwrap().hosts[&self._host].power) })
    }

    fn get_power_metrics<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::power::Power, RedfishError>> {
        Box::pin(async move { todo!() })
    }

    fn power<'a>(
        &'a self,
        action: libredfish::SystemPowerControl,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let power_state = match action {
                libredfish::SystemPowerControl::ForceOff
                | libredfish::SystemPowerControl::GracefulShutdown => PowerState::Off,
                _ => PowerState::On,
            };
            let mut state = self.state.lock().unwrap();
            let host_state = state.hosts.get_mut(&self._host).unwrap();
            host_state.power = power_state;
            host_state.actions.push(RedfishSimAction::Power(action));
            Ok(())
        })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    fn bmc_reset<'a>(
        &'a self,
        reset_type: Option<ManagerResetType>,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let host_state = state.hosts.get_mut(&self._host).unwrap();
            host_state
                .actions
                .push(RedfishSimAction::BmcReset(reset_type));
            Ok(())
        })
    }

    fn get_spx_nic_east_west_control_enabled<'a>(
        &'a self,
        _nic_index: u8,
    ) -> libredfish::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn set_spx_nic_east_west_control_enabled<'a>(
        &'a self,
        _nic_index: u8,
        _enabled: bool,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn get_spx_nic_mac_address<'a>(
        &'a self,
        _nic_index: u8,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn get_spx_nic_model_and_name<'a>(
        &'a self,
        _nic_index: u8,
    ) -> libredfish::RedfishFuture<'a, Result<Option<SpxNicModelAndName>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::thermal::Thermal, RedfishError>>
    {
        Box::pin(async move { todo!() })
    }

    fn machine_setup<'a>(
        &'a self,
        boot_interface: Option<libredfish::BootInterfaceRef<'a>>,
        _bios_profiles: &'a HashMap<
            libredfish::model::service_root::RedfishVendor,
            HashMap<
                String,
                HashMap<libredfish::BiosProfileType, HashMap<String, serde_json::Value>>,
            >,
        >,
        _profile_type: libredfish::BiosProfileType,
        oem_manager_profiles: &'a HashMap<
            libredfish::model::service_root::RedfishVendor,
            HashMap<
                String,
                HashMap<libredfish::BiosProfileType, HashMap<String, serde_json::Value>>,
            >,
        >,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let job_id = state.machine_setup_bios_job_id.clone();
            let host_state = state.hosts.get_mut(&self._host).unwrap();
            // `machine_setup` re-asserts the platform BIOS config, which
            // re-enables this host's `HttpDev1` HTTP-boot device. This is the
            // recovery a de-enumeration relies on: a subsequent
            // `set_boot_order_dpu_first` can then promote the device and the
            // boot order sticks. Per-host, so it recovers only this host.
            host_state.http_dev1_enabled = true;
            host_state
                .boot_interface_targets
                .push(boot_interface.map(RedfishSimBootInterfaceRef::from));
            host_state.actions.push(RedfishSimAction::MachineSetup {
                oem_manager_profiles: oem_manager_profiles.clone(),
                boot_interface_mac: boot_interface.map(boot_interface_ref_to_string),
            });
            Ok(job_id)
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        boot_interface: Option<libredfish::BootInterfaceRef<'a>>,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .machine_setup_status_targets
                .entry(self._host.clone())
                .or_default()
                .push(boot_interface.map(RedfishSimBootInterfaceRef::from));
            Ok(libredfish::MachineSetupStatus {
                is_done: true,
                diffs: vec![],
            })
        })
    }

    fn lockdown<'a>(
        &'a self,
        target: libredfish::EnabledDisabled,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let host_state = state.hosts.get_mut(&self._host).unwrap();
            host_state.lockdown = target;
            Ok(())
        })
    }

    fn lockdown_status<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::Status, RedfishError>> {
        Box::pin(async move {
            let state = self.state.lock().unwrap();
            Ok(libredfish::Status::build_fake(
                state.hosts[&self._host].lockdown,
            ))
        })
    }

    fn setup_serial_console<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { todo!() })
    }

    fn serial_console_status<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::Status, RedfishError>> {
        Box::pin(async move { todo!() })
    }

    fn get_boot_options<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::BootOptions, RedfishError>> {
        Box::pin(async move {
            Ok(libredfish::BootOptions {
                odata: Default::default(),
                description: None,
                members: vec![],
                name: "Boot Options".to_string(),
            })
        })
    }

    fn get_boot_option<'a>(
        &'a self,
        option_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::BootOption, RedfishError>> {
        Box::pin(async move {
            Ok(libredfish::model::BootOption {
                odata: Default::default(),
                alias: None,
                description: None,
                boot_option_enabled: None,
                boot_option_reference: String::new(),
                display_name: option_id.to_string(),
                id: option_id.to_string(),
                name: option_id.to_string(),
                uefi_device_path: None,
            })
        })
    }

    fn boot_once<'a>(
        &'a self,
        _target: libredfish::Boot,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn boot_first<'a>(
        &'a self,
        _target: libredfish::Boot,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn set_boot_override<'a>(
        &'a self,
        _settings: libredfish::BootOverride,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn clear_tpm<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { todo!() })
    }

    fn bios<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>>
    {
        Box::pin(async move {
            match self.state.lock().unwrap().bios_attributes.clone() {
                Some(attributes) => Ok(attributes),
                None => todo!(),
            }
        })
    }

    fn set_bios<'a>(
        &'a self,
        _values: HashMap<String, serde_json::Value>,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { todo!() })
    }

    fn pending<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>>
    {
        Box::pin(async move { todo!() })
    }

    fn clear_pending<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { todo!() })
    }

    fn pcie_devices<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<libredfish::PCIeDevice>, RedfishError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    fn change_password<'a>(
        &'a self,
        user: &'a str,
        new: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let s_user = user.to_string();
            let mut state = self.state.lock().unwrap();
            if let Some(message) = &state.change_password_error {
                return Err(RedfishError::GenericError {
                    error: message.clone(),
                });
            }
            if state.password_change_required {
                return Err(RedfishError::PasswordChangeRequired {
                    account_uri: state.password_change_required_account_uri.clone(),
                });
            }
            self.authorize(&mut state, "AccountService/Accounts")?;
            if !state.users.contains_key(&s_user) {
                return Err(RedfishError::UserNotFound(s_user));
            }
            if state.reject_password_reuse
                && state
                    .users
                    .get(&s_user)
                    .is_some_and(|current| current == new)
            {
                return Err(sim_http_error(
                    http::StatusCode::BAD_REQUEST,
                    "AccountService/Accounts",
                    "sim: new password must differ from current",
                ));
            }
            state.users.insert(s_user, new.to_string());
            Ok(())
        })
    }

    fn change_password_by_id<'a>(
        &'a self,
        account_id: &'a str,
        new_pass: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let s_acct = account_id.to_string();
            let mut state = self.state.lock().unwrap();
            if let Some(message) = &state.change_password_error {
                return Err(RedfishError::GenericError {
                    error: message.clone(),
                });
            }
            self.authorize(&mut state, "AccountService/Accounts")?;
            if !state.users.contains_key(&s_acct) {
                return Err(RedfishError::UserNotFound(s_acct));
            }
            if state.reject_password_reuse
                && state
                    .users
                    .get(&s_acct)
                    .is_some_and(|current| current == new_pass)
            {
                return Err(sim_http_error(
                    http::StatusCode::BAD_REQUEST,
                    "AccountService/Accounts",
                    "sim: new password must differ from current",
                ));
            }
            state.users.insert(s_acct, new_pass.to_string());
            Ok(())
        })
    }

    fn get_firmware<'a>(
        &'a self,
        id: &'a str,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move {
            if id == "Bluefield_FW_ERoT" {
                Ok(serde_json::from_str(
                    "{
            \"@odata.id\": \"/redfish/v1/UpdateService/FirmwareInventory/Bluefield_FW_ERoT\",
            \"@odata.type\": \"#SoftwareInventory.v1_4_0.SoftwareInventory\",
            \"Description\": \"Other image\",
            \"Id\": \"Bluefield_FW_ERoT\",
            \"Manufacturer\": \"NVIDIA\",
            \"Name\": \"Software Inventory\",
            \"Version\": \"00.02.0180.0000\"
            }",
                )
                .unwrap())
            } else if id == "DPU_NIC" {
                Ok(serde_json::from_str(
                    "{
            \"@odata.id\": \"/redfish/v1/UpdateService/FirmwareInventory/DPU_NIC\",
            \"@odata.type\": \"#SoftwareInventory.v1_4_0.SoftwareInventory\",
            \"Description\": \"Other image\",
            \"Id\": \"DPU_NIC\",
            \"Manufacturer\": \"NVIDIA\",
            \"Name\": \"Software Inventory\",
            \"Version\": \"32.39.2048\"
            }",
                )
                .unwrap())
            } else {
                let state = self.state.lock().unwrap();
                Ok(serde_json::from_str(
                    "{
            \"@odata.id\": \"/redfish/v1/UpdateService/FirmwareInventory/BMC_Firmware\",
            \"@odata.type\": \"#SoftwareInventory.v1_4_0.SoftwareInventory\",
            \"Description\": \"BMC image\",
            \"Id\": \"BMC_Firmware\",
            \"Name\": \"Software Inventory\",
            \"Updateable\": true,
            \"Version\": \"BF-FW-VERSION\",
            \"WriteProtected\": false
          }"
                    .replace("FW-VERSION", state.fw_version.as_str())
                    .as_str(),
                )
                .unwrap())
            }
        })
    }

    fn update_firmware<'a>(
        &'a self,
        _firmware: tokio::fs::File,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::task::Task, RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.fw_version = Arc::new("24.10-17".to_string());
            Ok(serde_json::from_str(
                "{
            \"@odata.id\": \"/redfish/v1/TaskService/Tasks/0\",
            \"@odata.type\": \"#Task.v1_4_3.Task\",
            \"Id\": \"0\"
            }",
            )
            .unwrap())
        })
    }

    fn update_firmware_simple_update<'a>(
        &'a self,
        _image_uri: &'a str,
        _targets: Vec<String>,
        _transfer_protocol: TransferProtocolType,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::task::Task, RedfishError>> {
        Box::pin(async move {
            Ok(serde_json::from_str(
                "{
            \"@odata.id\": \"/redfish/v1/TaskService/Tasks/0\",
            \"@odata.type\": \"#Task.v1_4_3.Task\",
            \"Id\": \"0\"
            }",
            )
            .unwrap())
        })
    }

    fn get_task<'a>(
        &'a self,
        id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::task::Task, RedfishError>> {
        Box::pin(async move {
            if self
                .state
                .lock()
                .unwrap()
                .get_task_trigger_evidence_returns_interrupted
                && id == TRIGGER_EVIDENCE_TASK_ID
            {
                return Ok(serde_json::from_str(
                    "{
                    \"@odata.id\": \"/redfish/v1/TaskService/Tasks/0\",
                    \"@odata.type\": \"#Task.v1_4_3.Task\",
                    \"Id\": \"0\",
                    \"PercentComplete\": 100,
                    \"StartTime\": \"2024-01-30T09:00:52+00:00\",
                    \"TaskMonitor\": \"/redfish/v1/TaskService/Tasks/0/Monitor\",
                    \"TaskState\": \"Interrupted\",
                    \"TaskStatus\": \"OK\"
                    }",
                )
                .unwrap());
            }
            Ok(serde_json::from_str(
                "{
            \"@odata.id\": \"/redfish/v1/TaskService/Tasks/0\",
            \"@odata.type\": \"#Task.v1_4_3.Task\",
            \"Id\": \"0\",
            \"PercentComplete\": 100,
            \"StartTime\": \"2024-01-30T09:00:52+00:00\",
            \"TaskMonitor\": \"/redfish/v1/TaskService/Tasks/0/Monitor\",
            \"TaskState\": \"Completed\",
            \"TaskStatus\": \"OK\"
            }",
            )
            .unwrap())
        })
    }

    fn get_chassis_all<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Ok(vec![
                "Bluefield_BMC".to_string(),
                "Bluefield_EROT".to_string(),
                "Card1".to_string(),
            ])
        })
    }

    fn get_chassis<'a>(
        &'a self,
        id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Chassis, RedfishError>> {
        Box::pin(async move {
            let state = self.state.lock().unwrap();
            let manufacturer = state
                .chassis_manufacturer
                .clone()
                .unwrap_or_else(|| "Nvidia".to_string());
            Ok(Chassis {
                manufacturer: Some(manufacturer),
                model: Some("Bluefield 3 SmartNIC Main Card".to_string()),
                name: Some("Card1".to_string()),
                network_adapters: (id == "Card1"
                    && !state.network_adapter_port_mac_addresses.is_empty())
                .then(|| ODataId {
                    odata_id: "/redfish/v1/Chassis/Card1/NetworkAdapters".to_string(),
                }),
                ..Default::default()
            })
        })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(vec!["NvidiaNetworkAdapter".to_string()]) })
    }

    fn get_chassis_network_adapter<'a>(
        &'a self,
        _chassis_id: &'a str,
        _id: &'a str,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::chassis::NetworkAdapter, RedfishError>,
    > {
        Box::pin(async move {
            Ok(serde_json::from_str(
                r##"
            {
                "@odata.id": "/redfish/v1/Chassis/Card1/NetworkAdapters/NvidiaNetworkAdapter",
                "@odata.type": "#NetworkAdapter.v1_9_0.NetworkAdapter",
                "Id": "NetworkAdapter",
                "Manufacturer": "Nvidia",
                "Name": "NvidiaNetworkAdapter",
                "NetworkDeviceFunctions": {
                  "@odata.id": "/redfish/v1/Chassis/Card1/NetworkAdapters/NvidiaNetworkAdapter/NetworkDeviceFunctions"
                },
                "Ports": {
                  "@odata.id": "/redfish/v1/Chassis/Card1/NetworkAdapters/NvidiaNetworkAdapter/Ports"
                }
              }
            "##)
                .unwrap())
        })
    }

    fn get_chassis_assembly<'a>(
        &'a self,
        _id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Assembly, RedfishError>> {
        Box::pin(async move { todo!() })
    }

    fn get_manager_ethernet_interfaces<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<std::string::String>, RedfishError>> {
        Box::pin(async move { Ok(vec!["eth0".to_string(), "vlan4040".to_string()]) })
    }

    fn get_manager_ethernet_interface<'a>(
        &'a self,
        _id: &'a str,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::ethernet_interface::EthernetInterface, RedfishError>,
    > {
        Box::pin(
            async move { Ok(libredfish::model::ethernet_interface::EthernetInterface::default()) },
        )
    }

    fn get_system_ethernet_interfaces<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<std::string::String>, RedfishError>> {
        Box::pin(async move { Ok(vec!["oob_net0".to_string()]) })
    }

    fn get_system_ethernet_interface<'a>(
        &'a self,
        _id: &'a str,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::ethernet_interface::EthernetInterface, RedfishError>,
    > {
        Box::pin(
            async move { Ok(libredfish::model::ethernet_interface::EthernetInterface::default()) },
        )
    }

    fn get_software_inventories<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<std::string::String>, RedfishError>> {
        Box::pin(async move {
            Ok(vec![
                "BMC_Firmware".to_string(),
                "Bluefield_FW_ERoT".to_string(),
                "DPU_NIC".to_string(),
            ])
        })
    }

    fn get_system<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::ComputerSystem, RedfishError>>
    {
        Box::pin(async move {
            let id = self
                .state
                .lock()
                .unwrap()
                .system_id
                .clone()
                .unwrap_or_else(|| "Bluefield".to_string());
            let chassis = self
                .state
                .lock()
                .unwrap()
                .system_chassis_ids
                .iter()
                .map(|id| ODataId {
                    odata_id: format!("/redfish/v1/Chassis/{id}"),
                })
                .collect::<Vec<_>>();
            Ok(libredfish::model::ComputerSystem {
                id,
                links: (!chassis.is_empty()).then_some(
                    libredfish::model::system::ComputerSystemLinks {
                        chassis: Some(chassis),
                        managed_by: None,
                    },
                ),
                boot_progress: Some(libredfish::model::BootProgress {
                    last_state: Some(libredfish::model::BootProgressTypes::OSRunning),
                    last_state_time: Some(Utc::now().to_string()),
                    oem_last_state: Some("OSRunning".to_string()),
                }),
                ..Default::default()
            })
        })
    }

    fn get_secure_boot<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::secure_boot::SecureBoot, RedfishError>,
    > {
        Box::pin(async move {
            let secure_boot_enabled = self
                .state
                .clone()
                .lock()
                .unwrap()
                .secure_boot
                .load(Ordering::Relaxed);
            Ok(libredfish::model::secure_boot::SecureBoot {
                odata: ODataLinks {
                    odata_context: None,
                    odata_id: "/redfish/v1/Systems/Bluefield/SecureBoot".to_string(),
                    odata_type: "#SecureBoot.v1_1_0.SecureBoot".to_string(),
                    odata_etag: None,
                    links: None,
                },
                id: "SecureBoot".to_string(),
                name: "UEFI Secure Boot".to_string(),
                secure_boot_current_boot: if secure_boot_enabled {
                    Some(EnabledDisabled::Enabled)
                } else {
                    Some(EnabledDisabled::Disabled)
                },
                secure_boot_enable: Some(secure_boot_enabled),
                secure_boot_mode: Some(SecureBootMode::UserMode),
            })
        })
    }

    fn disable_secure_boot<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn get_network_device_functions<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<std::string::String>, RedfishError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn get_network_device_function<'a>(
        &'a self,
        _chassis_id: &'a str,
        _id: &'a str,
        _port: Option<&'a str>,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::network_device_function::NetworkDeviceFunction, RedfishError>,
    > {
        Box::pin(async move {
            Ok(
                libredfish::model::network_device_function::NetworkDeviceFunction {
                    odata: None,
                    description: None,
                    id: None,
                    ethernet: None,
                    name: None,
                    net_dev_func_capabilities: Some(Vec::new()),
                    net_dev_func_type: None,
                    links: None,
                    oem: None,
                },
            )
        })
    }

    fn get_ports<'a>(
        &'a self,
        _chassis_id: &'a str,
        _network_adapter: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<std::string::String>, RedfishError>> {
        Box::pin(async move {
            let count = self
                .state
                .lock()
                .unwrap()
                .network_adapter_port_mac_addresses
                .len();
            Ok((0..count).map(|index| index.to_string()).collect())
        })
    }

    fn get_port<'a>(
        &'a self,
        _chassis_id: &'a str,
        _network_adapter: &'a str,
        id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::port::NetworkPort, RedfishError>>
    {
        Box::pin(async move {
            let index = id
                .parse::<usize>()
                .map_err(|error| RedfishError::GenericError {
                    error: format!("invalid simulated network adapter port ID {id}: {error}"),
                })?;
            let state = self.state.lock().unwrap();
            let mac_address = state
                .network_adapter_port_mac_addresses
                .get(index)
                .copied()
                .ok_or_else(|| RedfishError::GenericError {
                    error: format!("unknown simulated network adapter port ID {id}"),
                })?;
            Ok(libredfish::model::port::NetworkPort {
                odata: None,
                description: None,
                id: Some(id.to_string()),
                name: None,
                link_status: None,
                link_network_technology: None,
                current_speed_gbps: None,
                ethernet: Some(libredfish::model::port::PortEthernet {
                    associated_mac_addresses: vec![mac_address.to_string()],
                }),
                oem: None,
            })
        })
    }

    fn change_uefi_password<'a>(
        &'a self,
        _current_uefi_password: &'a str,
        _new_uefi_password: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            if let Some(message) = &self.state.lock().unwrap().uefi_password_change_error {
                return Err(RedfishError::GenericError {
                    error: message.clone(),
                });
            }
            Ok(None)
        })
    }

    fn change_boot_order<'a>(
        &'a self,
        _boot_array: Vec<String>,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        _role_id: libredfish::RoleId,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state.users.contains_key(username) {
                return Err(RedfishError::HTTPErrorCode {
                    url: "AccountService/Accounts".to_string(),
                    status_code: http::StatusCode::BAD_REQUEST,
                    response_body: format!(
                        r##"{{
                "UserName@Message.ExtendedInfo": [
                  {{
                    "@odata.type": "#Message.v1_1_1.Message",
                    "Message": "The requested resource of type ManagerAccount with the property UserName with the value {username} already exists.",
                    "MessageArgs": [
                      "ManagerAccount",
                      "UserName",
                      "{username}"
                    ],
                    "MessageId": "Base.1.15.0.ResourceAlreadyExists",
                    "MessageSeverity": "Critical",
                    "Resolution": "Do not repeat the create operation as the resource has already been created."
                  }}
                ]
              }}"##
                    ),
                });
            }

            state
                .users
                .insert(username.to_string(), password.to_string());
            Ok(())
        })
    }

    fn delete_user<'a>(
        &'a self,
        _username: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn get_service_root<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::service_root::ServiceRoot, RedfishError>,
    > {
        Box::pin(async move {
            let state = self.state.lock().unwrap();
            let vendor = state
                .service_root_vendor
                .clone()
                .unwrap_or_else(|| "Nvidia".to_string());
            let product = state
                .service_root_product
                .clone()
                .unwrap_or_else(|| "GB200 NVL".to_string());
            Ok(ServiceRoot {
                vendor: Some(vendor),
                product: Some(product),
                component_integrity: Some(ODataId {
                    odata_id: "Valid Data".to_string(),
                }),
                ..Default::default()
            })
        })
    }

    fn get_systems<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            // Check auth so credential fallback is observable.
            self.authorize(&mut state, "Systems")?;
            Ok(Vec::new())
        })
    }

    fn get_managers<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn get_manager<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<libredfish::model::Manager, RedfishError>> {
        Box::pin(async move {
            let mut manager: libredfish::model::Manager = serde_json::from_str(
                r##"{
            "@odata.id": "/redfish/v1/Managers/Bluefield_BMC",
            "@odata.type": "#Manager.v1_14_0.Manager",
            "Actions": {
              "#Manager.Reset": {
                "@Redfish.ActionInfo": "/redfish/v1/Managers/Bluefield_BMC/ResetActionInfo",
                "target": "/redfish/v1/Managers/Bluefield_BMC/Actions/Manager.Reset"
              },
              "#Manager.ResetToDefaults": {
                "ResetType@Redfish.AllowableValues": [
                  "ResetAll"
                ],
                "target": "/redfish/v1/Managers/Bluefield_BMC/Actions/Manager.ResetToDefaults"
              }
            },
            "CommandShell": {
              "ConnectTypesSupported": [
                "SSH"
              ],
              "MaxConcurrentSessions": 1,
              "ServiceEnabled": true
            },
            "DateTime": "2024-04-09T11:13:49+00:00",
            "DateTimeLocalOffset": "+00:00",
            "Description": "Baseboard Management Controller",
            "EthernetInterfaces": {
              "@odata.id": "/redfish/v1/Managers/Bluefield_BMC/EthernetInterfaces"
            },
            "FirmwareVersion": "bf-23.10-5-0-g87a8acd1708.1701259870.8631477",
            "GraphicalConsole": {
              "ConnectTypesSupported": [
                "KVMIP"
              ],
              "MaxConcurrentSessions": 4,
              "ServiceEnabled": true
            },
            "Id": "Bluefield_BMC",
            "LastResetTime": "2024-04-01T13:04:04+00:00",
            "LogServices": {
                "@odata.id": "/redfish/v1/Managers/Bluefield_BMC/LogServices"
              },
              "ManagerType": "BMC",
              "Model": "OpenBmc",
              "Name": "OpenBmc Manager",
              "NetworkProtocol": {
                "@odata.id": "/redfish/v1/Managers/Bluefield_BMC/NetworkProtocol"
              },
              "Oem": {
                "@odata.id": "/redfish/v1/Managers/Bluefield_BMC/Oem",
                "@odata.type": "#OemManager.Oem",
                "Nvidia": {
                  "@odata.id": "/redfish/v1/Managers/Bluefield_BMC/Oem/Nvidia"
                },
                "OpenBmc": {
                  "@odata.id": "/redfish/v1/Managers/Bluefield_BMC/Oem/OpenBmc",
                  "@odata.type": "#OemManager.OpenBmc",
                  "Certificates": {
                    "@odata.id": "/redfish/v1/Managers/Bluefield_BMC/Truststore/Certificates"
                  }
                }
              },
              "PowerState": "On",
              "SerialConsole": {
                "ConnectTypesSupported": [
                  "IPMI",
                  "SSH"
                ],
                "MaxConcurrentSessions": 15,
                "ServiceEnabled": true
              },
              "ServiceEntryPointUUID": "a614e837-6b4a-4560-8c22-c6ed1b96c7c9",
              "Status": {
                "Conditions": [],
                "Health": "OK",
                "HealthRollup": "OK",
                "State": "Starting"
              },
              "UUID": "0b623306-fa7f-42d2-809d-a63a13d49c8d"
        }"##,
            )
            .unwrap();
            // Update the date_time to current time for tests, applying any
            // configured offset so tests can simulate an out-of-sync BMC clock.
            let offset = self.state.lock().unwrap().bmc_time_offset_seconds;
            manager.date_time = Some(chrono::Utc::now() + chrono::Duration::seconds(offset));
            Ok(manager)
        })
    }

    fn bmc_reset_to_defaults<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn get_system_event_log<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<libredfish::model::sel::LogEntry>, RedfishError>>
    {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn get_bmc_event_log<'a>(
        &'a self,
        _from: Option<chrono::DateTime<chrono::Utc>>,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<libredfish::model::sel::LogEntry>, RedfishError>>
    {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if std::mem::take(&mut state.bmc_event_log_error_once) {
                return Err(RedfishError::GenericError {
                    error: "transient BMC event-log failure".to_string(),
                });
            }
            if state.bmc_event_log_supported {
                return Ok(Vec::new());
            }
            Err(RedfishError::NotSupported(
                "BMC Event Log not supported for tests".to_string(),
            ))
        })
    }

    fn get_tasks<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn add_secure_boot_certificate<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            Ok(Task {
                odata: ODataLinks {
                    odata_context: None,
                    odata_id: "odata_id".to_string(),
                    odata_type: "odata_type".to_string(),
                    odata_etag: None,
                    links: None,
                },
                id: "".to_string(),
                messages: Vec::new(),
                name: None,
                task_state: None,
                task_status: None,
                task_monitor: None,
                percent_complete: None,
            })
        })
    }

    fn enable_secure_boot<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.state
                .clone()
                .lock()
                .unwrap()
                .secure_boot
                .store(true, Ordering::Relaxed);
            Ok(())
        })
    }

    fn change_username<'a>(
        &'a self,
        _old_name: &'a str,
        _new_name: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }
    fn get_accounts<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<Vec<libredfish::model::account_service::ManagerAccount>, RedfishError>,
    > {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state.get_accounts_error {
                return Err(sim_http_error(
                    http::StatusCode::SERVICE_UNAVAILABLE,
                    "AccountService/Accounts",
                    "sim: forced get_accounts error",
                ));
            }
            // Reading the account collection is gated behind login on a real BMC,
            // so authorize the credential this client was created with (a no-op
            // unless enforcement is on).
            self.authorize(&mut state, "AccountService/Accounts")?;
            let accounts = state
                .users
                .keys()
                .map(|name| libredfish::model::account_service::ManagerAccount {
                    odata: libredfish::model::OData::default(),
                    id: Some(name.clone()),
                    username: name.clone(),
                    password: None,
                    role_id: "Administrator".to_string(),
                    name: None,
                    description: None,
                    enabled: Some(true),
                    locked: Some(false),
                })
                .collect();
            Ok(accounts)
        })
    }
    fn set_machine_password_policy<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }
    fn update_firmware_multipart<'a>(
        &'a self,
        _filename: &'a Path,
        _reboot: bool,
        _timeout: Duration,
        _component_type: ComponentType,
    ) -> libredfish::RedfishFuture<'a, Result<String, RedfishError>> {
        Box::pin(async move {
            // Simulate it taking a bit of time to upload
            tokio::time::sleep(Duration::from_secs(4)).await;
            Ok("0".to_string())
        })
    }

    fn get_job_state<'a>(
        &'a self,
        _job_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<JobState, RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            Ok(state
                .job_state_sequence
                .pop_front()
                .unwrap_or(JobState::Unknown))
        })
    }

    fn get_collection<'a>(
        &'a self,
        _id: ODataId,
    ) -> libredfish::RedfishFuture<'a, Result<Collection, RedfishError>> {
        Box::pin(async move {
            Ok(Collection {
                url: String::new(),
                body: HashMap::new(),
            })
        })
    }

    fn get_resource<'a>(
        &'a self,
        _id: ODataId,
    ) -> libredfish::RedfishFuture<'a, Result<Resource, RedfishError>> {
        Box::pin(async move {
            Ok(Resource {
                url: String::new(),
                raw: Default::default(),
            })
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: libredfish::BootInterfaceRef<'a>,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let host_state = state.hosts.get_mut(&self._host).unwrap();
            // Reordering only promotes an existing boot device; it can't
            // re-create a de-enumerated one. So the order reads as configured
            // exactly when this host's HTTP boot device is currently enabled.
            host_state.is_boot_order_setup = Some(host_state.http_dev1_enabled);
            host_state
                .boot_interface_targets
                .push(Some(RedfishSimBootInterfaceRef::from(boot_interface)));
            host_state
                .actions
                .push(RedfishSimAction::SetBootOrderDpuFirst {
                    boot_interface_mac: boot_interface_ref_to_string(boot_interface),
                });
            Ok(None)
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        _current_uefi_password: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn get_base_network_adapters<'a>(
        &'a self,
        _system_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    fn get_base_network_adapter<'a>(
        &'a self,
        _system_id: &'a str,
        _id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move {
            todo!();
        })
    }

    fn chassis_reset<'a>(
        &'a self,
        _chassis_id: &'a str,
        _reset_type: SystemPowerControl,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn get_update_service<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<UpdateService, RedfishError>> {
        Box::pin(async move {
            todo!();
        })
    }

    fn get_base_mac_address<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(Some("a088c208804c".to_string())) })
    }

    fn lockdown_bmc<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state.lockdown_bmc_applies.unwrap_or(true) {
                let host_state = state.hosts.get_mut(&self._host).unwrap();
                host_state.lockdown = target;
            }
            Ok(())
        })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move {
            todo!();
        })
    }

    fn get_drives_metrics<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<Drives>, RedfishError>> {
        Box::pin(async move {
            todo!();
        })
    }

    fn is_ipmi_over_lan_enabled<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { Ok(false) })
    }

    fn enable_ipmi_over_lan<'a>(
        &'a self,
        _target: EnabledDisabled,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn enable_rshim_bmc<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn clear_nvram<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn get_nic_mode<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Option<NicMode>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn set_nic_mode<'a>(
        &'a self,
        _mode: NicMode,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn enable_infinite_boot<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn is_infinite_boot_enabled<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn reset_bios<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn set_host_rshim<'a>(
        &'a self,
        _enabled: EnabledDisabled,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.state.lock().unwrap().platform_actions.push(
                RedfishSimPlatformAction::SetHostRshim {
                    host: self._host.clone(),
                },
            );
            Ok(())
        })
    }

    fn get_host_rshim<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Option<EnabledDisabled>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn set_idrac_lockdown<'a>(
        &'a self,
        _enabled: EnabledDisabled,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn get_boss_controller<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(self.state.lock().unwrap().boss_controller_id.clone()) })
    }

    fn decommission_storage_controller<'a>(
        &'a self,
        _controller_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn create_storage_volume<'a>(
        &'a self,
        _controller_id: &'a str,
        _volume_name: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: libredfish::BootInterfaceRef<'a>,
    ) -> libredfish::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let host_state = state.hosts.get_mut(&self._host).unwrap();
            // Readiness is per-host: it defaults to configured (`true`) and is
            // updated only by this host's own `set_boot_order_dpu_first` /
            // `set_is_boot_order_setup`, so other hosts can't flip it.
            let is_boot_order_setup = host_state.is_boot_order_setup.unwrap_or(true);
            host_state
                .boot_interface_targets
                .push(Some(RedfishSimBootInterfaceRef::from(boot_interface)));
            host_state.actions.push(RedfishSimAction::IsBootOrderSetup {
                boot_interface_mac: boot_interface_ref_to_string(boot_interface),
            });
            Ok(is_boot_order_setup)
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        boot_interface: Option<libredfish::BootInterfaceRef<'a>>,
    ) -> libredfish::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state
                .hosts
                .get_mut(&self._host)
                .unwrap()
                .boot_interface_targets
                .push(boot_interface.map(RedfishSimBootInterfaceRef::from));
            state
                .platform_actions
                .push(RedfishSimPlatformAction::IsBiosSetup {
                    host: self._host.clone(),
                });
            Ok(state.is_bios_setup.unwrap_or(true))
        })
    }

    fn get_secure_boot_certificate<'a>(
        &'a self,
        _database_id: &'a str,
        _certificate_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Certificate, RedfishError>> {
        Box::pin(async move {
            Ok(Certificate {
                certificate_string: String::new(),
                certificate_type: "PEM".to_string(),
                issuer: HashMap::new(),
                valid_not_before: String::new(),
                valid_not_after: String::new(),
            })
        })
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        _database_id: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(vec!["1".to_string()]) })
    }

    fn get_component_integrities<'a>(
        &'a self,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::component_integrity::ComponentIntegrities, RedfishError>,
    > {
        Box::pin(async move {
            if self.state.lock().unwrap().no_component_integrities {
                return Ok(ComponentIntegrities {
                    members: Vec::new(),
                    name: "ComponentIntegrities".to_string(),
                    count: 0,
                });
            }
            Ok(ComponentIntegrities {
                members: vec![ComponentIntegrity {
                    component_integrity_enabled: true,
                    component_integrity_type: "SPDM".to_string(),
                    component_integrity_type_version: "1.1.0".to_string(),
                    id: "ERoT_BMC_0".to_string(),
                    name: "SPDM Integrity for ERoT_BMC_0".to_string(),
                    target_component_uri: Some("/redfish/v1/Chassis/ERoT_BMC_0".to_string()),
                    spdm: Some(libredfish::model::component_integrity::SPDMData {
                        identity_authentication:
                            libredfish::model::component_integrity::IdentityAuthentication { responder_authentication: libredfish::model::component_integrity::ResponderAuthentication {
                                component_certificate: ODataId {
                                    odata_id:
                                        "/redfish/v1/Chassis/ERoT_BMC_0/Certificates/CertChain"
                                            .to_string(),
                                },
                            } },
                        requester: ODataId {
                            odata_id: "/redfish/v1/Managers/BMC_0".to_string(),
                        },
                    }),
                    actions: Some(libredfish::model::component_integrity::SPDMActions {
                        get_signed_measurements: Some(
                            libredfish::model::component_integrity::SPDMGetSignedMeasurements {
                                action_info: "/redfish/v1/ComponentIntegrity/ERoT_BMC_0/SPDMGetSignedMeasurementsActionInfo".to_string(),
                                target: "/redfish/v1/ComponentIntegrity/ERoT_BMC_0/Actions/ComponentIntegrity.SPDMGetSignedMeasurements".to_string(),
                            },
                        ),
                    }),
                    links: Some(
                        libredfish::model::component_integrity::ComponentsProtectedLinks {
                            components_protected: vec![ODataId{ odata_id: "/redfish/v1/Managers/BMC_0".to_string() }]
                        },
                    ),
                },
                ComponentIntegrity {
                    component_integrity_enabled: true,
                    component_integrity_type: "SPDM".to_string(),
                    component_integrity_type_version: "1.1.0".to_string(),
                    id: "HGX_IRoT_GPU_0".to_string(),
                    name: "SPDM Integrity for HGX_IRoT_GPU_0".to_string(),
                    target_component_uri: Some("/redfish/v1/Chassis/HGX_IRoT_GPU_0".to_string()),
                    spdm: Some(libredfish::model::component_integrity::SPDMData {
                        identity_authentication:
                            libredfish::model::component_integrity::IdentityAuthentication { responder_authentication: libredfish::model::component_integrity::ResponderAuthentication {
                                component_certificate: ODataId {
                                    odata_id:
                                        "/redfish/v1/Chassis/HGX_IRoT_GPU_0/Certificates/CertChain"
                                            .to_string(),
                                },
                            } },
                        requester: ODataId {
                            odata_id: "/redfish/v1/Managers/BMC_0".to_string(),
                        },
                    }),
                    actions: Some(libredfish::model::component_integrity::SPDMActions {
                        get_signed_measurements: Some(
                            libredfish::model::component_integrity::SPDMGetSignedMeasurements {
                                action_info: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_0/SPDMGetSignedMeasurementsActionInfo".to_string(),
                                target: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_0/Actions/ComponentIntegrity.SPDMGetSignedMeasurements".to_string(),
                            },
                        ),
                    }),
                    links: Some(
                        libredfish::model::component_integrity::ComponentsProtectedLinks {
                            components_protected: vec![ODataId{ odata_id: "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_0".to_string() }]
                        },
                    ),
                },
                ComponentIntegrity {
                    component_integrity_enabled: true,
                    component_integrity_type: "SPDM".to_string(),
                    component_integrity_type_version: "1.1.0".to_string(),
                    id: "HGX_IRoT_GPU_1".to_string(),
                    name: "SPDM Integrity for HGX_IRoT_GPU_1".to_string(),
                    target_component_uri: Some("/redfish/v1/Chassis/HGX_IRoT_GPU_1".to_string()),
                    spdm: Some(libredfish::model::component_integrity::SPDMData {
                        identity_authentication:
                            libredfish::model::component_integrity::IdentityAuthentication { responder_authentication: libredfish::model::component_integrity::ResponderAuthentication {
                                component_certificate: ODataId {
                                    odata_id:
                                        "/redfish/v1/Chassis/HGX_IRoT_GPU_1/Certificates/CertChain"
                                            .to_string(),
                                },
                            } },
                        requester: ODataId {
                            odata_id: "/redfish/v1/Managers/BMC_0".to_string(),
                        },
                    }),
                    actions: Some(libredfish::model::component_integrity::SPDMActions {
                        get_signed_measurements: Some(
                            libredfish::model::component_integrity::SPDMGetSignedMeasurements {
                                action_info: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/SPDMGetSignedMeasurementsActionInfo".to_string(),
                                target: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/Actions/ComponentIntegrity.SPDMGetSignedMeasurements".to_string(),
                            },
                        ),
                    }),
                    links: Some(
                        libredfish::model::component_integrity::ComponentsProtectedLinks {
                            components_protected: vec![ODataId{ odata_id: "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_1".to_string() }]
                        },
                    ),
                },
                ComponentIntegrity {
                    component_integrity_enabled: true,
                    component_integrity_type: "SPDM".to_string(),
                    component_integrity_type_version: "1.1.0".to_string(),
                    id: "HGX_IRoT_GPU_2".to_string(),
                    name: "SPDM Integrity for HGX_IRoT_GPU_2".to_string(),
                    target_component_uri: Some("/redfish/v1/Chassis/HGX_IRoT_GPU_2".to_string()),
                    spdm: Some(libredfish::model::component_integrity::SPDMData {
                        identity_authentication:
                            libredfish::model::component_integrity::IdentityAuthentication { responder_authentication: libredfish::model::component_integrity::ResponderAuthentication {
                                component_certificate: ODataId {
                                    odata_id:
                                        "/redfish/v1/Chassis/HGX_IRoT_GPU_2/Certificates/CertChain"
                                            .to_string(),
                                },
                            } },
                        requester: ODataId {
                            odata_id: "/redfish/v1/Managers/BMC_0".to_string(),
                        },
                    }),
                    actions: Some(libredfish::model::component_integrity::SPDMActions {
                        get_signed_measurements: Some(
                            libredfish::model::component_integrity::SPDMGetSignedMeasurements {
                                action_info: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_2/SPDMGetSignedMeasurementsActionInfo".to_string(),
                                target: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_2/Actions/ComponentIntegrity.SPDMGetSignedMeasurements".to_string(),
                            },
                        ),
                    }),
                    links: Some(
                        libredfish::model::component_integrity::ComponentsProtectedLinks {
                            components_protected: vec![ODataId{ odata_id: "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_2".to_string() }]
                        },
                    ),
                },
                ComponentIntegrity {
                    component_integrity_enabled: true,
                    component_integrity_type: "TPM".to_string(),
                    component_integrity_type_version: "1.1.0".to_string(),
                    id: "HGX_IRoT_GPU_1".to_string(),
                    name: "SPDM Integrity for HGX_IRoT_GPU_1".to_string(),
                    target_component_uri: Some("/redfish/v1/Chassis/HGX_IRoT_GPU_1".to_string()),
                    spdm: Some(libredfish::model::component_integrity::SPDMData {
                        identity_authentication:
                            libredfish::model::component_integrity::IdentityAuthentication { responder_authentication: libredfish::model::component_integrity::ResponderAuthentication {
                                component_certificate: ODataId {
                                    odata_id:
                                        "/redfish/v1/Chassis/HGX_IRoT_GPU_1/Certificates/CertChain"
                                            .to_string(),
                                },
                            } },
                        requester: ODataId {
                            odata_id: "/redfish/v1/Managers/BMC_0".to_string(),
                        },
                    }),
                    actions: Some(libredfish::model::component_integrity::SPDMActions {
                        get_signed_measurements: Some(
                            libredfish::model::component_integrity::SPDMGetSignedMeasurements {
                                action_info: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/SPDMGetSignedMeasurementsActionInfo".to_string(),
                                target: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/Actions/ComponentIntegrity.SPDMGetSignedMeasurements".to_string(),
                            },
                        ),
                    }),
                    links: Some(
                        libredfish::model::component_integrity::ComponentsProtectedLinks {
                            components_protected: vec![ODataId{ odata_id: "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_1".to_string() }]
                        },
                    ),
                },
                ComponentIntegrity {
                    component_integrity_enabled: false,
                    component_integrity_type: "SPDM".to_string(),
                    component_integrity_type_version: "1.1.0".to_string(),
                    id: "HGX_IRoT_GPU_1".to_string(),
                    name: "SPDM Integrity for HGX_IRoT_GPU_1".to_string(),
                    target_component_uri: Some("/redfish/v1/Chassis/HGX_IRoT_GPU_1".to_string()),
                    spdm: Some(libredfish::model::component_integrity::SPDMData {
                        identity_authentication:
                            libredfish::model::component_integrity::IdentityAuthentication { responder_authentication: libredfish::model::component_integrity::ResponderAuthentication {
                                component_certificate: ODataId {
                                    odata_id:
                                        "/redfish/v1/Chassis/HGX_IRoT_GPU_1/Certificates/CertChain"
                                            .to_string(),
                                },
                            } },
                        requester: ODataId {
                            odata_id: "/redfish/v1/Managers/BMC_0".to_string(),
                        },
                    }),
                    actions: Some(libredfish::model::component_integrity::SPDMActions {
                        get_signed_measurements: Some(
                            libredfish::model::component_integrity::SPDMGetSignedMeasurements {
                                action_info: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/SPDMGetSignedMeasurementsActionInfo".to_string(),
                                target: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/Actions/ComponentIntegrity.SPDMGetSignedMeasurements".to_string(),
                            },
                        ),
                    }),
                    links: Some(
                        libredfish::model::component_integrity::ComponentsProtectedLinks {
                            components_protected: vec![ODataId{ odata_id: "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_1".to_string() }]
                        },
                    ),
                },
                ComponentIntegrity {
                    component_integrity_enabled: true,
                    component_integrity_type: "SPDM".to_string(),
                    component_integrity_type_version: "0.1.0".to_string(),
                    id: "HGX_IRoT_GPU_1".to_string(),
                    name: "SPDM Integrity for HGX_IRoT_GPU_1".to_string(),
                    target_component_uri: Some("/redfish/v1/Chassis/HGX_IRoT_GPU_1".to_string()),
                    spdm: Some(libredfish::model::component_integrity::SPDMData {
                        identity_authentication:
                            libredfish::model::component_integrity::IdentityAuthentication { responder_authentication: libredfish::model::component_integrity::ResponderAuthentication {
                                component_certificate: ODataId {
                                    odata_id:
                                        "/redfish/v1/Chassis/HGX_IRoT_GPU_1/Certificates/CertChain"
                                            .to_string(),
                                },
                            } },
                        requester: ODataId {
                            odata_id: "/redfish/v1/Managers/BMC_0".to_string(),
                        },
                    }),
                    actions: Some(libredfish::model::component_integrity::SPDMActions {
                        get_signed_measurements: Some(
                            libredfish::model::component_integrity::SPDMGetSignedMeasurements {
                                action_info: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/SPDMGetSignedMeasurementsActionInfo".to_string(),
                                target: "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_1/Actions/ComponentIntegrity.SPDMGetSignedMeasurements".to_string(),
                            },
                        ),
                    }),
                    links: Some(
                        libredfish::model::component_integrity::ComponentsProtectedLinks {
                            components_protected: vec![ODataId{ odata_id: "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_1".to_string() }]
                        },
                    ),
                },
                ],
                name: "ComponentIntegrities".to_string(),
                count: 7,
            })
        })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        component_integrity_id: &'a str,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move {
            if self.state.lock().unwrap().firmware_for_component_error {
                return Err(RedfishError::GenericError {
                    error: "Firmware for Component Error".to_string(),
                });
            }
            if !component_integrity_id.contains("HGX_IRoT_GPU_") {
                return Err(RedfishError::NotSupported(
                    "not supported device".to_string(),
                ));
            }
            Ok(SoftwareInventory {
                odata: ODataLinks {
                    odata_context: None,
                    odata_id: "/redfish/v1/UpdateService/FirmwareInventory/HGX_FW_GPU_0"
                        .to_string(),
                    odata_type: "#SoftwareInventory.v1_4_0.SoftwareInventory".to_string(),
                    odata_etag: None,
                    links: None,
                },
                description: None,
                id: component_integrity_id.to_string(),
                version: Some("97.00.82.00.5F".to_string()),
                release_date: None,
            })
        })
    }

    fn get_component_ca_certificate<'a>(
        &'a self,
        _url: &'a str,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::component_integrity::CaCertificate, RedfishError>,
    > {
        Box::pin(async move {
            Ok(serde_json::from_str(r#"{
    "@odata.id": "/redfish/v1/Chassis/HGX_IRoT_GPU_0/Certificates/CertChain",
    "@odata.type": "Certificate.v1_5_0.Certificate",
    "CertificateString": "-----BEGIN CERTIFICATE-----\nMIIDdDCCAvqgAwIBAgIUdgzUdmT3058TdKflDS6w/mP3ps3F9n3TLq8GZw3U9tiL3T57skQBoIL\nTssh8Q5sdh+fdbgkiawE0IKvw26uFwIwZ0UBCk+3B6JuSijznMdCaX+lwxJ0Eq7V\nSFpkQATVveySG/Qo8NreDDAfu5dAcVBr\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICjjCCAhWgAwIBAgIJQMW6N4r97aTmMAoGCCqGSM49BAMDMFcxKzApBgNVBAMM\nIk5WSURJQSBHQjEwMCBQcm92aXNpb25lciBJQ0EgMDAwMDAxGzAZBgNVBAoMEk5W\nSURJQSBDb3Jwb3JhdGlvbjELMAkGA1UEBhMCVVMwIBcNMjMwNjIwMDAwMDAwWhgP\nOTk5OTEyMzEyMzU5NTlaMGQxGzAZBgNVBAUTEjQwQzVCQTM3OEFGREVEQTRFNjEL\nMAkGA1UEBhMCVVMxGzAZBgNVBAoMEk5WSURJQSBDb3Jwb3JhdGlvbjEbMBkGA1UE\nAwwSR0IxMDAgQTAxIEZTUCBCUk9NMHYwEAYHKoZIzj0CAQYFK4EEACIDYgAE4j9u\nVBS3aGs3+UXZz0zjA75rR4+vZ/dmSi077kPcErBP7TeY82L2YfmaEpB2H/aEw9x3\n8aTby9x+920rG9NN+8O8CBKzQW7YBpwGFUkmnLtcN34cMEw2gwUGTEvdtPfdo4Gd\nMIGaMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgIEMDcGCCsGAQUFBwEB\nBCswKTAnBggrBgEFBQcwAYYbaHR0cDovL29jc3AubmRpcy5udmlkaWEuY29tMB0G\nA1UdDgQWBBSRs+v751iHdsbshaYSkL+OTRhnfTAfBgNVHSMEGDAWgBQD78BUvvHZ\nTb1ls+d0V1ySn+B2RTAKBggqhkjOPQQDAwNnADBkAjANWRl8oyEkvYEk2KOY6YgS\nesPo7Wjnvpox3fLIk6FCxcX0Zirezk1T6COhPIK95PACMG5JPYssNlWpjeWOLs5x\nkyAyW2sgtXU9RKxm6i8lmjWyXG3odPVUF8F12CaIxTp5eg==\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICrjCCAjOgAwIBAgIQXYBfwgLOvCcgRkD8IC+BhTAKBggqhkjOPQQDAzA9MR4w\nHAYDVQQDDBVOVklESUEgR0IxMDAgSWRlbnRpdHkxGzAZBgNVBAoMEk5WSURJQSBD\nb3Jwb3JhdGlvbjAgFw0yMzA2MjAwMDAwMDBaGA85OTk5MTIzMTIzNTk1OVowVzEr\nMCkGA1UEAwwiTlZJRElBIEdCMTAwIFByb3Zpc2lvbmVyIElDQSAwMDAwMDEbMBkG\nA1UECgwSTlZJRElBIENvcnBvcmF0aW9uMQswCQYDVQQGEwJVUzB2MBAGByqGSM49\nAgEGBSuBBAAiA2IABBdKHmiD7JKUIKnyKTdLazbcVBj9HMpHaOE9nEcQvoeoZeHn\nV1Gc+SwOvxtMl7tckYLx4BQLEs/AXWYx0hAVleVP3krbeIfWtmEwsPa9IQQ4APpH\nOYZp9QwBoYHNcci9c6OB2zCB2DAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQE\nAwIBBjA8BgNVHR8ENTAzMDGgL6AthitodHRwOi8vY3JsLm5kaXMubnZpZGlhLmNv\nbS9jcmwvbDItZ2IxMDAuY3JsMDcGCCsGAQUFBwEBBCswKTAnBggrBgEFBQcwAYYb\naHR0cDovL29jc3AubmRpcy5udmlkaWEuY29tMB0GA1UdDgQWBBQD78BUvvHZTb1l\ns+d0V1ySn+B2RTAfBgNVHSMEGDAWgBTtqWR9ZFo/Pa3Guetkw1uSG6TgAjAKBggq\nhkjOPQQDAwNpADBmAjEA8M2NglY92IX9SQrtvdfMTxl4A02CqLHZeleuBHgRX7Mn\n5C7jfE5c23Ejl0j1JnB1AjEAt+tHqjht6MbZJtLX/09pFnFgcTHG0erYR8v375gq\niC3QSP6Khjum4ukzH0KV6JRm\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICijCCAhCgAwIBAgIQV7ceDOVWAwo2pOUrTKlfHjAKBggqhkjOPQQDAzA1MSIw\nIAYDVQQDDBlOVklESUEgRGV2aWNlIElkZW50aXR5IENBMQ8wDQYDVQQKDAZOVklE\nSUEwIBcNMjMwMTAxMDAwMDAwWhgPOTk5OTEyMzEyMzU5NTlaMD0xHjAcBgNVBAMM\nFU5WSURJQSBHQjEwMCBJZGVudGl0eTEbMBkGA1UECgwSTlZJRElBIENvcnBvcmF0\naW9uMHYwEAYHKoZIzj0CAQYFK4EEACIDYgAE/XKlEaBWlqMDj+rpBFEjY2LYS+Ja\niRyYigtuUNpFRia3nsWoBwewhLA1wrw56KAGDXInX5Yde14hqPXCgjUzNkbN5mrC\nmya7oXdUtVYA186E9LlPsm8YEwiPaDd/3Vl8o4HaMIHXMA8GA1UdEwEB/wQFMAMB\nAf8wDgYDVR0PAQH/BAQDAgEGMDsGA1UdHwQ0MDIwMKAuoCyGKmh0dHA6Ly9jcmwu\nbmRpcy5udmlkaWEuY29tL2NybC9sMS1yb290LmNybDA3BggrBgEFBQcBAQQrMCkw\nJwYIKwYBBQUHMAGGG2h0dHA6Ly9vY3NwLm5kaXMubnZpZGlhLmNvbTAdBgNVHQ4E\nFgQU7alkfWRaPz2txrnrZMNbkhuk4AIwHwYDVR0jBBgwFoAUV4X/g/JjzGV9aLc6\nW/SNSsv7SV8wCgYIKoZIzj0EAwMDaAAwZQIwSDCBZ6OhBe4gV1ueWUwYAeDI/LAj\nS8GSEh5PxCwiHMs1EYcOGlCX2e/RlJ8lDFuGAjEAwFOOiBjvktWQP8Fgj7hGefny\nJPhnEXLwVYUemI4ejiPsua4GKin56ip9ZoEHdBUQ\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIICCzCCAZCgAwIBAgIQLTZwscoQBBHB/sDoKgZbVDAKBggqhkjOPQQDAzA1MSIw\nIAYDVQQDDBlOVklESUEgRGV2aWNlIElkZW50aXR5IENBMQ8wDQYDVQQKDAZOVklE\nSUEwIBcNMjExMTA1MDAwMDAwWhgPOTk5OTEyMzEyMzU5NTlaMDUxIjAgBgNVBAMM\nGU5WSURJQSBEZXZpY2UgSWRlbnRpdHkgQ0ExDzANBgNVBAoMBk5WSURJQTB2MBAG\nByqGSM49AgEGBSuBBAAiA2IABA5MFKM7+KViZljbQSlgfky/RRnEQScW9NDZF8SX\ngAW96r6u/Ve8ZggtcYpPi2BS4VFu6KfEIrhN6FcHG7WP05W+oM+hxj7nyA1r1jkB\n2Ry70YfThX3Ba1zOryOP+MJ9vaNjMGEwDwYDVR0TAQH/BAUwAwEB/zAOBgNVHQ8B\nAf8EBAMCAQYwHQYDVR0OBBYEFFeF/4PyY8xlfWi3Olv0jUrL+0lfMB8GA1UdIwQY\nMBaAFFeF/4PyY8xlfWi3Olv0jUrL+0lfMAoGCCqGSM49BAMDA2kAMGYCMQCPeFM3\nTASsKQVaT+8S0sO9u97PVGCpE9d/I42IT7k3UUOLSR/qvJynVOD1vQKVXf0CMQC+\nEY55WYoDBvs2wPAH1Gw4LbcwUN8QCff8bFmV4ZxjCRr4WXTLFHBKjbfneGSBWwA=\n-----END CERTIFICATE-----\n",
    "CertificateType": "PEMchain",
    "CertificateUsageTypes": [
        "Device"
    ],
    "Id": "CertChain",
    "Name": "HGX_IRoT_GPU_0 Certificate Chain",
    "SPDM": {
        "SlotId": 0
    }
}"#).unwrap())
        })
    }

    fn trigger_evidence_collection<'a>(
        &'a self,
        _url: &'a str,
        _nonce: &'a str,
    ) -> libredfish::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            let task_str = format!(
                r##"{{
                    "@odata.id": "/redfish/v1/TaskService/Tasks/{TRIGGER_EVIDENCE_TASK_ID}",
                    "@odata.type": "#Task.v1_4_3.Task",
                    "Id": "{TRIGGER_EVIDENCE_TASK_ID}"
                }}"##
            );
            Ok(serde_json::from_str(&task_str).unwrap())
        })
    }

    fn get_evidence<'a>(
        &'a self,
        _url: &'a str,
    ) -> libredfish::RedfishFuture<
        'a,
        Result<libredfish::model::component_integrity::Evidence, RedfishError>,
    > {
        Box::pin(async move {
            Ok(serde_json::from_str(r#"{
  "HashingAlgorithm": "TPM_ALG_SHA_512",
  "SignedMeasurements": "EeAB/81ALklRkZ0fn8F7O77CNxHPOc8qUBSxyklrCAUYJkkLATUAATIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABxanBrNxxfwfICAJzQ0008O0greTQqXk737JD0VEpjAwAAJiAwRSQU+6KuRrawestxwit0TbmColQFu1wvCp+l1Iwchz0xEfaiI6r4lmCUk5tL0DPnBnYBurQrNIrqqwk5G1C+H5VW25T+N/B+8oojcVByle4LCq6pubLivQGKAYPb",
  "SigningAlgorithm": "TPM_ALG_ECDSA_ECC_NIST_P384",
  "Version": "1.1.0"
}"#).unwrap())
        })
    }

    fn set_host_privilege_level<'a>(
        &'a self,
        _level: HostPrivilegeLevel,
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.state.lock().unwrap().platform_actions.push(
                RedfishSimPlatformAction::SetHostPrivilegeLevel {
                    host: self._host.clone(),
                },
            );
            Ok(())
        })
    }

    fn set_utc_timezone<'a>(&'a self) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .hosts
                .get_mut(&self._host)
                .unwrap()
                .actions
                .push(RedfishSimAction::SetUtcTimezone);
            Ok(())
        })
    }

    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> libredfish::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .hosts
                .get_mut(&self._host)
                .unwrap()
                .actions
                .push(RedfishSimAction::SetNtpServers(servers.to_vec()));
            Ok(())
        })
    }
}

#[async_trait]
impl RedfishClientPool for RedfishSim {
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        {
            let mut state = self.state.lock().unwrap();
            state.create_client_calls.push(CreateClientCall {
                host: host.to_string(),
                vendor,
            });
            if let Some(error) = state.create_client_error.clone() {
                return Err(RedfishClientCreationError::RedfishError(
                    RedfishError::GenericError { error },
                ));
            }
            let default_lockdown = state.default_lockdown.unwrap_or(EnabledDisabled::Disabled);
            state
                .hosts
                .entry(host.to_string())
                .or_insert(RedfishSimHostState {
                    lockdown: default_lockdown,
                    ..Default::default()
                });
            if state.fw_version.is_empty() {
                state.fw_version = Arc::new("24.10-17".to_string());
            }
        }
        Ok(Box::new(RedfishSimClient {
            state: self.state.clone(),
            _host: host.to_string(),
            _port: port,
            auth,
        }))
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        &self.credential_manager
    }
}

impl super::sealed::Sealed for RedfishSim {}

#[async_trait]
impl super::BmcCredentialOps for RedfishSim {
    async fn uefi_setup(
        &self,
        _access: &carbide_utils::redfish::BmcAccessInfo,
        dpu: bool,
        _sitewide_uefi_credentials: carbide_secrets::credentials::Credentials,
    ) -> Result<Option<String>, super::CredentialOpError> {
        let mut state = self.state.lock().unwrap();
        // Fail before recording the action: the op never reached the device.
        if let Some(error) = state.uefi_setup_client_creation_error.clone() {
            return Err(super::CredentialOpError::ClientCreation(
                RedfishClientCreationError::RedfishError(RedfishError::GenericError { error }),
            ));
        }
        state
            .platform_actions
            .push(RedfishSimPlatformAction::UefiSetup { dpu });
        Ok(None)
    }
}
