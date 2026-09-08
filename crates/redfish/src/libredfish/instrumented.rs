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

//! Per-operation RED metrics for every Redfish client the pool creates.
//!
//! [`InstrumentedRedfish`] decorates a [`Redfish`] client so that each trait
//! method records the shared outbound-call triad from
//! [`carbide_instrument::red`]:
//! `carbide_external_call_duration_milliseconds{backend = "redfish",
//! operation, outcome}`. The pool wraps every client it hands out
//! ([`super::implementation`]), so the one decorator covers every consumer
//! -- machine-controller, spdm-controller, site-explorer, preingestion, and
//! the rest -- without touching their call sites. The `operation` label is
//! the trait method's own name, or a fixed compile-time label for a direct
//! operation that needs response metadata unavailable through the trait. It
//! is never a URL or other wire data.
//!
//! This backend's `outcome` has a third value beyond the shared helper's
//! ok/error: `unsupported`, for calls a vendor answers with a local
//! [`RedfishError::NotSupported`] stub -- an expected answer, not an
//! external-call failure (see [`instrumented_redfish`]).
//!
//! Two kinds of methods are written out by hand rather than by the
//! delegation macro. Password-bearing operations redact their password
//! arguments from the error *before* the RED helper writes its failure WARN
//! -- call sites redact only after the error has come back to them, which
//! would be too late for that log line. And
//! [`Redfish::ac_powercycle_supported_by_power`] is a local capability
//! check with no BMC I/O to meter, so it delegates plainly.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use carbide_instrument::red;
use libredfish::model::account_service::ManagerAccount;
use libredfish::model::certificate::Certificate;
use libredfish::model::component_integrity::{CaCertificate, ComponentIntegrities, Evidence};
use libredfish::model::oem::nvidia_dpu::{HostPrivilegeLevel, NicMode};
use libredfish::model::power::Power;
use libredfish::model::secure_boot::SecureBoot;
use libredfish::model::sel::LogEntry;
use libredfish::model::sensor::GPUSensors;
use libredfish::model::service_root::ServiceRoot;
use libredfish::model::software_inventory::SoftwareInventory;
use libredfish::model::storage::Drives;
use libredfish::model::task::Task;
use libredfish::model::thermal::Thermal;
use libredfish::model::update_service::{ComponentType, TransferProtocolType, UpdateService};
use libredfish::model::{BootOption, ComputerSystem, Manager, ODataId};
use libredfish::standard::RedfishStandard;
use libredfish::{
    Assembly, BiosProfileType, BiosProfileVendor, Boot, BootInterfaceRef, BootOptions,
    BootOverride, Chassis, Collection, EnabledDisabled, EthernetInterface, JobState,
    MachineSetupStatus, ManagerResetType, NetworkAdapter, NetworkDeviceFunction, NetworkPort,
    PCIeDevice, PowerState, Redfish, RedfishError, RedfishFuture, Resource, RoleId,
    SpxNicModelAndName, Status, SystemPowerControl,
};

use super::redact_password;

/// The `backend` label every Redfish external call records under.
pub(super) const REDFISH_BACKEND: &str = "redfish";

/// A [`Redfish`] client whose every call records the RED triad.
pub(super) struct InstrumentedRedfish {
    inner: Box<dyn Redfish>,
}

impl InstrumentedRedfish {
    pub(super) fn new(inner: Box<dyn Redfish>) -> Self {
        Self { inner }
    }
}

/// [`redact_password`], skipping the empty string: `str::replace` with an
/// empty needle would garble the message instead of redacting anything, and
/// an empty current password is a real input (`uefi_setup` probes with one).
fn redact_nonempty(error: RedfishError, password: &str) -> RedfishError {
    if password.is_empty() {
        error
    } else {
        redact_password(error, password)
    }
}

/// Redacts two passwords with union masking: matches that touch or overlap
/// in the text (one password containing the other, or the two sharing a
/// boundary, like `foobar`/`foo` or `abcdef`/`defghi` in `abcdefghi`) redact
/// as one merged span, so a sequential replace can never leave a fragment of
/// either password behind for the failure WARN.
fn redact_both(error: RedfishError, a: &str, b: &str) -> RedfishError {
    super::redact_passwords(error, &[a, b])
}

/// Times a single Redfish call on the shared RED instrument, with the outcome
/// vocabulary this backend needs: a [`RedfishError::NotSupported`] answer
/// records `outcome = "unsupported"` and logs nothing. Vendors answer
/// capability probes (lockdown status, boot options) with local
/// `NotSupported` stubs as a matter of course, so the refusal is data the
/// caller owns -- counting it as `error` would inflate the failure rate with
/// expected answers, and a WARN per probe would flood the logs fleet-wide.
/// The split also shows, per operation, what a fleet's BMCs don't support.
/// Everything else matches [`red::instrumented`]: `ok` records silently,
/// real failures record `error` and write the same single WARN.
async fn instrumented_redfish<T>(
    operation: &'static str,
    call: impl Future<Output = Result<T, RedfishError>>,
) -> Result<T, RedfishError> {
    let started = Instant::now();
    let result = call.await;
    let outcome = match &result {
        Ok(_) => "ok",
        Err(RedfishError::NotSupported(_)) => "unsupported",
        Err(_) => "error",
    };
    red::record(
        REDFISH_BACKEND,
        operation,
        outcome,
        started.elapsed().as_secs_f64() * 1_000.0,
    );
    if let Err(error) = &result
        && !matches!(error, RedfishError::NotSupported(_))
    {
        tracing::warn!(backend = REDFISH_BACKEND, operation, error = %error, "external call failed");
    }
    result
}

/// Generates the delegating trait methods: each one passes its arguments to
/// the inner client and records the RED triad with the method's own name as
/// the `operation` label. Entries are the trait's signatures with the return
/// type written as the `Ok` type only; the macro restores the
/// `RedfishFuture<Result<_, RedfishError>>` shell.
macro_rules! delegate_with_red {
    ($(
        fn $method:ident<$lt:lifetime>(
            & $selflt:lifetime self
            $(, $arg:ident : $ty:ty )* $(,)?
        ) -> $ok:ty;
    )+) => {
        $(
            fn $method<$lt>(
                & $selflt self
                $(, $arg : $ty )*
            ) -> RedfishFuture<$lt, Result<$ok, RedfishError>> {
                Box::pin(instrumented_redfish(
                    stringify!($method),
                    self.inner.$method($( $arg ),*),
                ))
            }
        )+
    };
}

impl Redfish for InstrumentedRedfish {
    fn std_redfish(&self) -> &RedfishStandard {
        self.inner.std_redfish()
    }

    delegate_with_red! {
        fn change_username<'a>(&'a self, old_name: &'a str, new_name: &'a str) -> ();
        fn get_accounts<'a>(&'a self) -> Vec<ManagerAccount>;
        fn delete_user<'a>(&'a self, username: &'a str) -> ();
        fn get_firmware<'a>(&'a self, id: &'a str) -> SoftwareInventory;
        fn get_software_inventories<'a>(&'a self) -> Vec<String>;
        fn get_tasks<'a>(&'a self) -> Vec<String>;
        fn get_task<'a>(&'a self, id: &'a str) -> Task;
        fn get_power_state<'a>(&'a self) -> PowerState;
        fn get_service_root<'a>(&'a self) -> ServiceRoot;
        fn get_systems<'a>(&'a self) -> Vec<String>;
        fn get_system<'a>(&'a self) -> ComputerSystem;
        fn get_managers<'a>(&'a self) -> Vec<String>;
        fn get_manager<'a>(&'a self) -> Manager;
        fn get_secure_boot<'a>(&'a self) -> SecureBoot;
        fn disable_secure_boot<'a>(&'a self) -> ();
        fn enable_secure_boot<'a>(&'a self) -> ();
        fn get_secure_boot_certificate<'a>(
            &'a self,
            database_id: &'a str,
            certificate_id: &'a str,
        ) -> Certificate;
        fn get_secure_boot_certificates<'a>(&'a self, database_id: &'a str) -> Vec<String>;
        fn add_secure_boot_certificate<'a>(
            &'a self,
            pem_cert: &'a str,
            database_id: &'a str,
        ) -> Task;
        fn get_power_metrics<'a>(&'a self) -> Power;
        fn power<'a>(&'a self, action: SystemPowerControl) -> ();
        fn bmc_reset<'a>(&'a self, reset_type: Option<ManagerResetType>) -> ();
        fn chassis_reset<'a>(
            &'a self,
            chassis_id: &'a str,
            reset_type: SystemPowerControl,
        ) -> ();
        fn bmc_reset_to_defaults<'a>(&'a self) -> ();
        fn get_thermal_metrics<'a>(&'a self) -> Thermal;
        fn get_gpu_sensors<'a>(&'a self) -> Vec<GPUSensors>;
        fn get_system_event_log<'a>(&'a self) -> Vec<LogEntry>;
        fn get_bmc_event_log<'a>(
            &'a self,
            from: Option<chrono::DateTime<chrono::Utc>>,
        ) -> Vec<LogEntry>;
        fn get_drives_metrics<'a>(&'a self) -> Vec<Drives>;
        fn machine_setup<'a>(
            &'a self,
            boot_interface: Option<BootInterfaceRef<'a>>,
            bios_profiles: &'a BiosProfileVendor,
            selected_profile: BiosProfileType,
            oem_manager_profiles: &'a BiosProfileVendor,
        ) -> Option<String>;
        fn machine_setup_status<'a>(
            &'a self,
            boot_interface: Option<BootInterfaceRef<'a>>,
        ) -> MachineSetupStatus;
        fn is_bios_setup<'a>(&'a self, boot_interface: Option<BootInterfaceRef<'a>>) -> bool;
        fn set_machine_password_policy<'a>(&'a self) -> ();
        fn lockdown<'a>(&'a self, target: EnabledDisabled) -> ();
        fn lockdown_status<'a>(&'a self) -> Status;
        fn setup_serial_console<'a>(&'a self) -> ();
        fn serial_console_status<'a>(&'a self) -> Status;
        fn get_boot_options<'a>(&'a self) -> BootOptions;
        fn get_boot_option<'a>(&'a self, option_id: &'a str) -> BootOption;
        fn boot_once<'a>(&'a self, target: Boot) -> ();
        fn boot_first<'a>(&'a self, target: Boot) -> ();
        fn set_boot_override<'a>(&'a self, settings: BootOverride) -> Option<String>;
        fn change_boot_order<'a>(&'a self, boot_array: Vec<String>) -> ();
        fn clear_tpm<'a>(&'a self) -> ();
        fn pcie_devices<'a>(&'a self) -> Vec<PCIeDevice>;
        fn update_firmware<'a>(&'a self, filename: tokio::fs::File) -> Task;
        fn update_firmware_multipart<'a>(
            &'a self,
            firmware: &'a Path,
            reboot: bool,
            timeout: Duration,
            component_type: ComponentType,
        ) -> String;
        fn update_firmware_simple_update<'a>(
            &'a self,
            image_uri: &'a str,
            targets: Vec<String>,
            transfer_protocol: TransferProtocolType,
        ) -> Task;
        fn bios<'a>(&'a self) -> HashMap<String, serde_json::Value>;
        fn set_bios<'a>(&'a self, values: HashMap<String, serde_json::Value>) -> ();
        fn reset_bios<'a>(&'a self) -> ();
        fn pending<'a>(&'a self) -> HashMap<String, serde_json::Value>;
        fn clear_pending<'a>(&'a self) -> ();
        fn get_network_device_functions<'a>(&'a self, chassis_id: &'a str) -> Vec<String>;
        fn get_network_device_function<'a>(
            &'a self,
            chassis_id: &'a str,
            id: &'a str,
            port: Option<&'a str>,
        ) -> NetworkDeviceFunction;
        fn get_chassis_all<'a>(&'a self) -> Vec<String>;
        fn get_chassis<'a>(&'a self, id: &'a str) -> Chassis;
        fn get_chassis_assembly<'a>(&'a self, chassis_id: &'a str) -> Assembly;
        fn get_chassis_network_adapters<'a>(&'a self, chassis_id: &'a str) -> Vec<String>;
        fn get_chassis_network_adapter<'a>(
            &'a self,
            chassis_id: &'a str,
            id: &'a str,
        ) -> NetworkAdapter;
        fn get_base_network_adapters<'a>(&'a self, system_id: &'a str) -> Vec<String>;
        fn get_base_network_adapter<'a>(
            &'a self,
            system_id: &'a str,
            id: &'a str,
        ) -> NetworkAdapter;
        fn get_ports<'a>(
            &'a self,
            chassis_id: &'a str,
            network_adapter: &'a str,
        ) -> Vec<String>;
        fn get_port<'a>(
            &'a self,
            chassis_id: &'a str,
            network_adapter: &'a str,
            id: &'a str,
        ) -> NetworkPort;
        fn get_manager_ethernet_interfaces<'a>(&'a self) -> Vec<String>;
        fn get_manager_ethernet_interface<'a>(&'a self, id: &'a str) -> EthernetInterface;
        fn get_system_ethernet_interfaces<'a>(&'a self) -> Vec<String>;
        fn get_system_ethernet_interface<'a>(&'a self, id: &'a str) -> EthernetInterface;
        fn get_job_state<'a>(&'a self, job_id: &'a str) -> JobState;
        fn get_resource<'a>(&'a self, id: ODataId) -> Resource;
        fn get_collection<'a>(&'a self, id: ODataId) -> Collection;
        fn set_boot_order_dpu_first<'a>(
            &'a self,
            boot_interface: BootInterfaceRef<'a>,
        ) -> Option<String>;
        fn get_update_service<'a>(&'a self) -> UpdateService;
        fn get_base_mac_address<'a>(&'a self) -> Option<String>;
        fn lockdown_bmc<'a>(&'a self, target: EnabledDisabled) -> ();
        fn is_ipmi_over_lan_enabled<'a>(&'a self) -> bool;
        fn enable_ipmi_over_lan<'a>(&'a self, target: EnabledDisabled) -> ();
        fn enable_rshim_bmc<'a>(&'a self) -> ();
        fn clear_nvram<'a>(&'a self) -> ();
        fn get_nic_mode<'a>(&'a self) -> Option<NicMode>;
        fn set_nic_mode<'a>(&'a self, mode: NicMode) -> ();
        fn enable_infinite_boot<'a>(&'a self) -> ();
        fn is_infinite_boot_enabled<'a>(&'a self) -> Option<bool>;
        fn set_host_rshim<'a>(&'a self, enabled: EnabledDisabled) -> ();
        fn get_host_rshim<'a>(&'a self) -> Option<EnabledDisabled>;
        fn set_idrac_lockdown<'a>(&'a self, enabled: EnabledDisabled) -> ();
        fn get_boss_controller<'a>(&'a self) -> Option<String>;
        fn decommission_storage_controller<'a>(
            &'a self,
            controller_id: &'a str,
        ) -> Option<String>;
        fn create_storage_volume<'a>(
            &'a self,
            controller_id: &'a str,
            volume_name: &'a str,
        ) -> Option<String>;
        fn is_boot_order_setup<'a>(&'a self, boot_interface: BootInterfaceRef<'a>) -> bool;
        fn get_component_integrities<'a>(&'a self) -> ComponentIntegrities;
        fn get_firmware_for_component<'a>(
            &'a self,
            component_integrity_id: &'a str,
        ) -> SoftwareInventory;
        fn get_component_ca_certificate<'a>(&'a self, url: &'a str) -> CaCertificate;
        fn trigger_evidence_collection<'a>(&'a self, url: &'a str, nonce: &'a str) -> Task;
        fn get_evidence<'a>(&'a self, url: &'a str) -> Evidence;
        fn set_host_privilege_level<'a>(&'a self, level: HostPrivilegeLevel) -> ();
        fn set_utc_timezone<'a>(&'a self) -> ();
        fn set_ntp_servers<'a>(&'a self, servers: &'a [String]) -> ();
        fn get_spx_nic_east_west_control_enabled<'a>(&'a self, nic_index: u8) -> Option<bool>;
        fn set_spx_nic_east_west_control_enabled<'a>(&'a self, nic_index: u8, enabled: bool) -> ();
        fn get_spx_nic_mac_address<'a>(&'a self, nic_index: u8) -> Option<String>;
        fn get_spx_nic_model_and_name<'a>(&'a self, nic_index: u8) -> Option<SpxNicModelAndName>;
    }

    // MARK: - Password-bearing operations
    //
    // These redact their password arguments from the error before returning
    // it, so the RED helper's failure WARN (and everything downstream) only
    // ever sees the redacted form. Callers that redact again find nothing
    // left to replace.

    fn change_password<'a>(
        &'a self,
        username: &'a str,
        new_pass: &'a str,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(instrumented_redfish("change_password", async move {
            self.inner
                .change_password(username, new_pass)
                .await
                .map_err(|error| redact_nonempty(error, new_pass))
        }))
    }

    fn change_password_by_id<'a>(
        &'a self,
        account_id: &'a str,
        new_pass: &'a str,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(instrumented_redfish("change_password_by_id", async move {
            self.inner
                .change_password_by_id(account_id, new_pass)
                .await
                .map_err(|error| redact_nonempty(error, new_pass))
        }))
    }

    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        role_id: RoleId,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(instrumented_redfish("create_user", async move {
            self.inner
                .create_user(username, password, role_id)
                .await
                .map_err(|error| redact_nonempty(error, password))
        }))
    }

    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(instrumented_redfish("change_uefi_password", async move {
            self.inner
                .change_uefi_password(current_uefi_password, new_uefi_password)
                .await
                .map_err(|error| redact_both(error, current_uefi_password, new_uefi_password))
        }))
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(instrumented_redfish("clear_uefi_password", async move {
            self.inner
                .clear_uefi_password(current_uefi_password)
                .await
                .map_err(|error| redact_nonempty(error, current_uefi_password))
        }))
    }

    // MARK: - Local capability check

    fn ac_powercycle_supported_by_power(&self) -> bool {
        // No BMC I/O happens here, so there is no external call to meter.
        self.inner.ac_powercycle_supported_by_power()
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::MetricsCapture;
    use carbide_secrets::credentials::{CredentialKey, CredentialType};

    use super::*;
    use crate::libredfish::test_support::RedfishSim;
    use crate::libredfish::{RedfishAuth, RedfishClientPool};

    async fn sim_client(sim: &RedfishSim) -> InstrumentedRedfish {
        let client = sim
            .create_client(
                "localhost",
                None,
                RedfishAuth::Key(CredentialKey::HostRedfish {
                    credential_type: CredentialType::SiteDefault,
                }),
                None,
            )
            .await
            .expect("sim client");
        InstrumentedRedfish::new(client)
    }

    #[tokio::test]
    async fn ok_call_records_the_external_call_histogram() {
        let sim = RedfishSim::default();
        let client = sim_client(&sim).await;

        let metrics = MetricsCapture::start();
        assert_eq!(
            client.get_power_state().await.expect("sim power state"),
            PowerState::On,
        );

        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "redfish"),
                    ("operation", "get_power_state"),
                    ("outcome", "ok"),
                ],
            ),
            1,
        );
    }

    #[tokio::test]
    async fn failed_call_records_the_error_outcome_and_returns_the_error() {
        let sim = RedfishSim::default();
        let client = sim_client(&sim).await;

        let metrics = MetricsCapture::start();
        let error = client
            .change_password_by_id("2", "site_pass")
            .await
            .expect_err("no account id 2 is seeded");
        assert!(
            matches!(&error, RedfishError::UserNotFound(id) if id == "2"),
            "expected UserNotFound(\"2\"), got {error:?}",
        );

        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "redfish"),
                    ("operation", "change_password_by_id"),
                    ("outcome", "error"),
                ],
            ),
            1,
        );
    }

    /// The three-way outcome split: a vendor's local `NotSupported` answer
    /// records `unsupported` with no log line, a genuine failure records
    /// `error` with the single WARN, and both errors propagate untouched.
    #[test]
    fn unsupported_answers_record_their_own_outcome_and_stay_quiet() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");
        let metrics = MetricsCapture::start();
        let logs = carbide_instrument::testing::capture_logs(|| {
            rt.block_on(async {
                instrumented_redfish::<()>(
                    "lockdown_status",
                    std::future::ready(Err(RedfishError::NotSupported(
                        "vendor answers locally".to_string(),
                    ))),
                )
                .await
                .expect_err("the refusal still propagates");

                instrumented_redfish::<()>(
                    "lockdown_status",
                    std::future::ready(Err(RedfishError::GenericError {
                        error: "boom".to_string(),
                    })),
                )
                .await
                .expect_err("the error still propagates");
            });
        });

        assert_eq!(
            logs.iter()
                .filter(|log| log.message == "external call failed")
                .count(),
            1,
            "only the genuine failure warns; unsupported stays quiet, got {logs:?}",
        );
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "redfish"),
                    ("operation", "lockdown_status"),
                    ("outcome", "unsupported"),
                ],
            ),
            1,
        );
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "redfish"),
                    ("operation", "lockdown_status"),
                    ("outcome", "error"),
                ],
            ),
            1,
        );
    }

    /// The containment case: one password contains the other; a naive
    /// shorter-first replace would fragment the longer one and leak its tail.
    #[test]
    fn redact_both_survives_contained_passwords() {
        let error = RedfishError::GenericError {
            error: "rejected foobar, and foo separately".to_string(),
        };
        let redacted = redact_both(error, "foobar", "foo");
        assert!(
            matches!(
                &redacted,
                RedfishError::GenericError { error } if error == "rejected REDACTED, and REDACTED separately"
            ),
            "both passwords must redact fully with no fragments, got {redacted:?}",
        );
    }

    /// The partial-overlap case: the two passwords share a boundary in the
    /// text, so any sequential replace leaks a fragment of one of them; the
    /// union mask redacts the merged span as one.
    #[test]
    fn redact_both_survives_partially_overlapping_passwords() {
        let error = RedfishError::GenericError {
            error: "rejected abcdefghi".to_string(),
        };
        let redacted = redact_both(error, "abcdef", "defghi");
        assert!(
            matches!(
                &redacted,
                RedfishError::GenericError { error } if error == "rejected REDACTED"
            ),
            "the overlapping span must redact as one with no fragments, got {redacted:?}",
        );
    }

    #[test]
    fn redact_nonempty_redacts_only_a_nonempty_password() {
        let error = RedfishError::GenericError {
            error: "rejected pass123".to_string(),
        };
        let redacted = redact_nonempty(error, "pass123");
        assert!(
            matches!(&redacted, RedfishError::GenericError { error } if error == "rejected REDACTED"),
            "expected the password redacted, got {redacted:?}",
        );

        let error = RedfishError::GenericError {
            error: "boom".to_string(),
        };
        let untouched = redact_nonempty(error, "");
        assert!(
            matches!(&untouched, RedfishError::GenericError { error } if error == "boom"),
            "an empty password must leave the message untouched, got {untouched:?}",
        );
    }
}
