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

//! Capability operations on a connected BMC.
//!
//! `bmc.power()?.set(ResetType::On)` runs the power driver selection chose
//! for this BMC; callers never see drivers or operation contexts.

use std::sync::Arc;

use bmc_drivers::CatalogError;
use bmc_platform::{
    Accounts, Attestation, Bios, BiosSettings, BiosStatus, BmcControl, BootInterfaceSelector,
    BootOrder, BootOrderStatus, Capability, ClassifyBmcError, Console, ConsoleSpec, ConsoleStatus,
    Dpu, DpuStatus, DriverOutcome, EvidenceProgress, Firmware, FirmwareUpload, HostPrivilegeLevel,
    Lockdown, LockdownDesiredState, LockdownScope, LockdownStatus, ManagerSettings,
    ManagerSettingsStatus, NicMode, OperationReference, PlatformError, Power, RshimState,
    SecureBoot, SecureBootStatus, Storage, SuperNic, TimeStatus,
};
use nv_redfish::Bmc;
use nv_redfish::account::ManagerAccountCreate;
use nv_redfish::resource::{PowerState, ResetType};
use nv_redfish::schema::certificate::Certificate;
use nv_redfish::schema::component_integrity::ComponentIntegrity;
use nv_redfish::schema::computer_system::BootUpdate;
use nv_redfish::schema::manager_account::ManagerAccount;
use nv_redfish::schema::secure_boot::SecureBootUpdate;
use nv_redfish::schema::software_inventory::SoftwareInventory;
use nv_redfish::schema::update_service::UpdateServiceSimpleUpdateAction;

use crate::ConnectedBmc;
use crate::observe::{driver_unavailable, observe};

/// The driver selected for one capability of a connected BMC.
///
/// Each operation builds a fresh operation context, so it observes the BMC's
/// current state rather than resources an earlier operation resolved.
pub struct BoundDriver<'a, D: ?Sized + 'a, B: Bmc + 'static> {
    driver: &'a D,
    bmc: &'a ConnectedBmc<B>,
}

impl<B: Bmc + 'static> ConnectedBmc<B>
where
    B::Error: ClassifyBmcError,
{
    /// Binds `capability`'s selected driver, reporting why there is none.
    fn bind<'a, D: ?Sized>(
        &'a self,
        capability: Capability,
        driver: Result<&'a D, CatalogError>,
    ) -> Result<BoundDriver<'a, D, B>, CatalogError> {
        driver
            .map(|driver| BoundDriver { driver, bmc: self })
            .inspect_err(|error| driver_unavailable(self.endpoint(), capability, error))
    }
}

/// Generates a capability's accessor on [`ConnectedBmc`] and its operations
/// on [`BoundDriver`]. `$capability` is the [`Capability`] wire name, and each
/// operation runs its driver method under [`observe`] as
/// `$capability.$method`.
macro_rules! capability_operations {
    ($capability:ident: $trait:ident {
        $(fn $method:ident(&self $(, $arg:ident: $ty:ty)* $(,)?) -> $ok:ty;)*
    }) => {
        impl<B: Bmc + 'static> ConnectedBmc<B>
        where
            B::Error: ClassifyBmcError,
        {
            #[doc = concat!("The [`", stringify!($trait), "`] driver selected for this BMC.")]
            pub fn $capability(&self) -> Result<BoundDriver<'_, dyn $trait<B>, B>, CatalogError> {
                self.bind(Capability::$trait, self.drivers().$capability())
            }
        }

        impl<B: Bmc + 'static> BoundDriver<'_, dyn $trait<B>, B>
        where
            B::Error: ClassifyBmcError,
        {
            $(
                #[doc = concat!("See [`", stringify!($trait), "::", stringify!($method), "`].")]
                pub async fn $method(&self $(, $arg: $ty)*) -> Result<$ok, PlatformError> {
                    observe(
                        self.bmc.endpoint(),
                        Some(Capability::$trait),
                        concat!(stringify!($capability), ".", stringify!($method)),
                        self.driver.$method(&self.bmc.operation_context() $(, $arg)*),
                    )
                    .await
                }
            )*
        }

        #[cfg(test)]
        mod $capability {
            #[test]
            fn operation_labels_use_the_capability_wire_name() {
                assert_eq!(super::Capability::$trait.as_str(), stringify!($capability));
            }
        }
    };
}

capability_operations!(accounts: Accounts {
    fn list(&self) -> Vec<Arc<ManagerAccount>>;
    fn create(&self, request: ManagerAccountCreate) -> DriverOutcome;
    fn delete(&self, username: &str) -> DriverOutcome;
    fn change_password(&self, account_username: &str, password: &str) -> DriverOutcome;
    fn change_username(&self, old_username: &str, new_username: &str) -> DriverOutcome;
    fn apply_default_policy(&self, account_username: &str, password: &str) -> DriverOutcome;
});

capability_operations!(attestation: Attestation {
    fn components(&self) -> Vec<Arc<ComponentIntegrity>>;
    fn firmware_for_component(&self, component_id: &str) -> Arc<SoftwareInventory>;
    fn ca_certificate(&self, component_id: &str) -> Arc<Certificate>;
    fn request_evidence(&self, component_id: &str, nonce: &[u8]) -> EvidenceProgress;
    fn poll_evidence(&self, pending: &OperationReference) -> EvidenceProgress;
});

capability_operations!(bios: Bios {
    fn apply(
        &self,
        profile: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> DriverOutcome;
    fn status(
        &self,
        profile: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> BiosStatus;
    fn reset(&self) -> DriverOutcome;
    fn clear_pending(&self) -> DriverOutcome;
    fn clear_nvram(&self) -> DriverOutcome;
    fn change_uefi_password(&self, current_password: &str, new_password: &str) -> DriverOutcome;
    fn clear_uefi_password(&self, current_password: &str) -> DriverOutcome;
    fn clear_tpm(&self) -> DriverOutcome;
    fn infinite_boot_enabled(&self) -> Option<bool>;
});

capability_operations!(bmc_control: BmcControl {
    fn reset(&self) -> DriverOutcome;
    fn reset_to_factory_defaults(&self) -> DriverOutcome;
    fn set_ntp_servers(&self, servers: &[String]) -> DriverOutcome;
    fn set_utc_timezone(&self) -> DriverOutcome;
    fn time_status(&self) -> TimeStatus;
    fn ipmi_over_lan_enabled(&self) -> bool;
    fn set_ipmi_over_lan(&self, enabled: bool) -> DriverOutcome;
    fn apply_settings(&self, profile: &ManagerSettings) -> DriverOutcome;
    fn settings_status(&self) -> ManagerSettingsStatus;
});

capability_operations!(boot_order: BootOrder {
    fn status(&self, boot_interface_selector: &BootInterfaceSelector) -> BootOrderStatus;
    fn set_override(&self, override_setting: &BootUpdate) -> DriverOutcome;
    fn configure(&self, boot_interface_selector: &BootInterfaceSelector) -> DriverOutcome;
});

capability_operations!(console: Console {
    fn setup(&self) -> DriverOutcome;
    fn status(&self) -> ConsoleStatus;
    fn spec(&self) -> ConsoleSpec;
});

capability_operations!(dpu: Dpu {
    fn status(&self) -> DpuStatus;
    fn set_nic_mode(&self, mode: NicMode) -> DriverOutcome;
    fn set_host_rshim(&self, state: RshimState) -> DriverOutcome;
    fn enable_bmc_rshim(&self) -> DriverOutcome;
    fn set_host_privilege_level(&self, level: HostPrivilegeLevel) -> DriverOutcome;
});

capability_operations!(firmware: Firmware {
    fn inventory(&self) -> Vec<Arc<SoftwareInventory>>;
    fn multipart_update(&self, upload: FirmwareUpload) -> DriverOutcome;
    fn simple_update(&self, request: &UpdateServiceSimpleUpdateAction) -> DriverOutcome;
});

capability_operations!(lockdown: Lockdown {
    fn status(&self) -> LockdownStatus;
    fn set(&self, scope: LockdownScope, desired: LockdownDesiredState) -> DriverOutcome;
});

capability_operations!(power: Power {
    fn state(&self) -> Option<PowerState>;
    fn ac_power_cycle_supported(&self) -> bool;
    fn set(&self, reset_type: ResetType) -> DriverOutcome;
    fn chassis_reset(&self, chassis_id: &str, reset_type: ResetType) -> DriverOutcome;
});

capability_operations!(secure_boot: SecureBoot {
    fn status(&self) -> SecureBootStatus;
    fn set(&self, update: &SecureBootUpdate) -> DriverOutcome;
    fn has_platform_key(&self) -> bool;
    fn add_platform_key(&self, pem: &str) -> DriverOutcome;
});

capability_operations!(storage: Storage {
    fn boot_controller(&self) -> Option<String>;
    fn decommission_controller(&self, controller_id: &str) -> DriverOutcome;
    fn create_volume(&self, controller_id: &str, volume_name: &str) -> DriverOutcome;
});

capability_operations!(super_nic: SuperNic {
    fn east_west_control(&self, index: u8) -> Option<bool>;
    fn set_east_west_control(&self, index: u8, enabled: bool) -> DriverOutcome;
    fn mac_address(&self, index: u8) -> Option<String>;
});
