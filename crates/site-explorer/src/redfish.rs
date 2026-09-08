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
use std::collections::HashMap;
use std::error::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use carbide_network::deserialize_input_mac_to_address;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::conv::{IntoModel, bmc_vendor};
use carbide_redfish::libredfish::dpu_bios::is_dpu_bios_attributes_not_ready;
use carbide_redfish::libredfish::{BmcCredentialOps, RedfishAuth, RedfishClientCreationError};
use carbide_redfish::nv_redfish::NvRedfishClientPool;
use carbide_secrets::credentials::Credentials;
use libredfish::model::ODataId;
use libredfish::model::oem::nvidia_dpu::NicMode;
use libredfish::model::service_root::RedfishVendor;
use libredfish::{Redfish, RedfishError};
use mac_address::MacAddress;
use model::errors::{ErrorCode, ErrorSubsystem, OperatorError, OperatorErrorSchema};
use model::machine_boot_interface::MachineBootInterfaceTarget;
use model::site_explorer::{
    BootOption, BootOrder, Chassis, ComputerSystem, ComputerSystemAttributes,
    EndpointExplorationError, EndpointExplorationReport, EndpointType, EthernetInterface,
    InternalLockdownStatus, Inventory, LockdownStatus, MachineSetupDiff, MachineSetupStatus,
    Manager, NetworkAdapter, PCIeDevice, SecureBootStatus, Service, UefiDevicePath,
};
use regex::Regex;

const NOT_FOUND: u16 = 404;
const BF4_NDF0_TO_BASE_MAC_OFFSET: u64 = 0x10;

// RedfishClient is a wrapper around a redfish client pool and implements redfish utility functions that the site explorer utilizes.
// TODO: In the future, we should refactor a lot of this client's work to api/src/redfish.rs because other components in carbide can utilize this functionality.
// Eventually, this file should only have code related to generating the site exploration report.
#[derive(Clone)]
pub(super) struct RedfishClient {
    redfish_client_pool: Arc<dyn BmcCredentialOps>,
    nv_redfish_client_pool: Arc<NvRedfishClientPool>,
}

impl RedfishClient {
    pub(super) fn new(
        redfish_client_pool: Arc<dyn BmcCredentialOps>,
        nv_redfish_client_pool: Arc<NvRedfishClientPool>,
    ) -> Self {
        Self {
            redfish_client_pool,
            nv_redfish_client_pool,
        }
    }

    async fn create_redfish_client(
        &self,
        bmc_ip_address: SocketAddr,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        self.redfish_client_pool
            .create_client(
                &bmc_ip_address.ip().to_string(),
                Some(bmc_ip_address.port()),
                auth,
                vendor,
            )
            .await
    }

    async fn create_anon_redfish_client(
        &self,
        bmc_ip_address: SocketAddr,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        // This currently uses a "standard" client without any vendor
        // specific implementations. If we end up ever needing vendor
        // specific support for a caller using this, we could simply
        // just drop in vendor: Option<RedfishVendor> to support an
        // override of using RedfishVendor::Unknown.
        self.create_redfish_client(
            bmc_ip_address,
            RedfishAuth::Anonymous,
            Some(RedfishVendor::Unknown),
        )
        .await
    }

    async fn create_direct_redfish_client(
        &self,
        bmc_ip_address: SocketAddr,
        Credentials::UsernamePassword { username, password }: Credentials,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        self.create_redfish_client(
            bmc_ip_address,
            RedfishAuth::Direct(username, password),
            vendor,
        )
        .await
    }

    async fn create_authenticated_redfish_client(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        self.create_direct_redfish_client(bmc_ip_address, credentials, None)
            .await
    }

    pub(super) async fn get_redfish_product(
        &self,
        bmc_ip_address: SocketAddr,
    ) -> Result<Option<String>, EndpointExplorationError> {
        let client = self
            .create_anon_redfish_client(bmc_ip_address)
            .await
            .map_err(map_redfish_client_creation_error)?;

        let service_root = client.get_service_root().await.map_err(map_redfish_error)?;

        Ok(service_root.product)
    }

    pub(super) async fn get_redfish_vendor(
        &self,
        bmc_ip_address: SocketAddr,
    ) -> Result<RedfishVendor, EndpointExplorationError> {
        let client = self
            .create_anon_redfish_client(bmc_ip_address)
            .await
            .map_err(map_redfish_client_creation_error)?;

        let service_root = client.get_service_root().await.map_err(map_redfish_error)?;

        // Do not gate on the raw `Vendor` field: some BMCs (e.g. Supermicro
        // SYS-121H-TNR) leave ServiceRoot.Vendor null but still identify
        // themselves via the `Oem` key. libredfish's `vendor()` already
        // consults Oem as a fallback, so resolve through it and only reject
        // when the result is genuinely unrecognized. See NVBug 6338388.
        match service_root.vendor() {
            Some(vendor) if vendor != RedfishVendor::Unknown => Ok(vendor),
            _ => {
                // Capture the raw vendor string the ServiceRoot actually reported
                // (the `Vendor` field, falling back to the first `Oem` key) so the
                // recorded exploration error says *what* we read and *where* from.
                // `None` here means the BMC reported neither — usually transient
                // while it is still initializing; `Some(_)` means a vendor we don't
                // recognize yet. See NVBug 6036327.
                let observed = service_root.vendor_string();
                tracing::info!(
                    %bmc_ip_address,
                    observed_vendor = ?observed,
                    "BMC ServiceRoot did not report a recognized vendor"
                );
                Err(EndpointExplorationError::MissingVendor { observed })
            }
        }
    }

    /// Probe the DPU model from the unauthenticated Redfish service root `Product` field.
    ///
    /// BlueField BMCs populate `ServiceRoot.Product` with a human-readable model string
    /// (e.g. `"BlueField-3 DPU"`). This makes a single anonymous `/redfish/v1` call and
    /// parses that field. Returns `DpuModel::Unknown` on any error or unrecognized string
    /// so callers can fall back to the catch-all factory credential.
    pub(super) async fn get_dpu_model_hint(
        &self,
        bmc_ip_address: SocketAddr,
    ) -> ::bmc_vendor::DpuModel {
        let client = match self.create_anon_redfish_client(bmc_ip_address).await {
            Ok(c) => c,
            Err(_) => return ::bmc_vendor::DpuModel::Unknown,
        };
        let service_root = match client.get_service_root().await {
            Ok(s) => s,
            Err(_) => return ::bmc_vendor::DpuModel::Unknown,
        };
        service_root
            .product
            .as_deref()
            .map(::bmc_vendor::DpuModel::from_service_root_product)
            .unwrap_or_default()
    }

    pub(super) async fn validate_bmc_credentials(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_direct_redfish_client(bmc_ip_address, credentials, Some(RedfishVendor::Unknown))
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.get_systems().await.map_err(map_redfish_error)?;

        Ok(())
    }

    /// Resolve the precise `RedfishVendor` of a BMC using the supplied
    /// credentials, delegating to the shared `RedfishClientPool` probe (an
    /// anonymous service-root read with a credentialed chassis-manufacturer
    /// fallback for BMCs that don't populate the service-root vendor).
    pub(super) async fn probe_bmc_vendor(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<RedfishVendor, EndpointExplorationError> {
        self.redfish_client_pool
            .probe_bmc_vendor(
                &bmc_ip_address.ip().to_string(),
                Some(bmc_ip_address.port()),
                credentials,
            )
            .await
            .map_err(map_redfish_client_creation_error)
    }

    pub(super) async fn set_bmc_root_password(
        &self,
        bmc_ip_address: SocketAddr,
        vendor: RedfishVendor,
        current_bmc_root_credentials: Credentials,
        new_password: String,
    ) -> Result<(), EndpointExplorationError> {
        // The two-client rotation flow (uninitialized `Unknown` client for the
        // `/AccountService` PATCH, then a vendor-specific client for the
        // password policy) and the per-vendor dispatch now live on the shared
        // `RedfishClientPool` primitive so credential rotation can reuse them.
        // See its docs for why the rotation PATCH must not initialize the
        // vendor client first.
        self.redfish_client_pool
            .set_bmc_root_password(
                &bmc_ip_address.ip().to_string(),
                Some(bmc_ip_address.port()),
                vendor,
                current_bmc_root_credentials,
                new_password,
            )
            .await
            .map_err(map_redfish_client_creation_error)
    }

    pub(super) async fn set_bf4_dpu_service_password(
        &self,
        bmc_ip_address: SocketAddr,
        root_credentials: Credentials,
        new_password: String,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client_pool
            .set_bf4_dpu_service_password(
                &bmc_ip_address.ip().to_string(),
                Some(bmc_ip_address.port()),
                root_credentials,
                new_password,
            )
            .await
            .map_err(map_redfish_client_creation_error)
    }

    pub(super) async fn generate_exploration_report(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface: Option<&BootInterfaceTarget>,
        vendor: Option<RedfishVendor>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        let client = self
            .create_direct_redfish_client(bmc_ip_address, credentials, vendor)
            .await
            .map_err(map_redfish_client_creation_error)?;

        let service_root = client.get_service_root().await.map_err(map_redfish_error)?;
        let redfish_vendor = service_root.vendor();
        // Lenovo XCC is the platform where we have verified that adapter Ports
        // belong to the linked host chassis. Keep that policy here so the
        // inventory path stays generic.
        let supports_adapter_port_mac_inventory = redfish_vendor == Some(RedfishVendor::Lenovo);
        let vendor = redfish_vendor.map(bmc_vendor);

        let manager = fetch_manager(client.as_ref())
            .await
            .map_err(map_redfish_error)?;
        let FetchedSystem {
            system,
            is_dpu,
            is_host,
            linked_chassis_ids,
        } = fetch_system(client.as_ref()).await?;

        let fetch_network_adapter_ports = should_fetch_network_adapter_ports(
            supports_adapter_port_mac_inventory,
            is_host,
            &linked_chassis_ids,
        );
        // TODO (spyda): once we test the BMC reset logic, we can enhance our logic here
        // to detect cases where the host's BMC is returning invalid (empty) chassis information, even though
        // an error is not returned.
        let FetchedChassis { chassis } = fetch_chassis(
            client.as_ref(),
            fetch_network_adapter_ports.then_some(linked_chassis_ids.as_slice()),
        )
        .await
        .map_err(map_redfish_error)?;
        let service = fetch_service(client.as_ref())
            .await
            .map_err(map_redfish_error)?;
        let (machine_setup_status, remediation_error) = match fetch_machine_setup_status(
            client.as_ref(),
            boot_interface,
        )
        .await
        {
            Ok(status) => (Some(status), None),
            Err(error) if is_dpu && is_dpu_bios_attributes_not_ready(&error) => {
                let details = format!(
                    "DPU BMC BIOS attributes not ready ({error}); scheduling a force-restart to mitigate the known UEFI POST/BMC race"
                );
                let exploration_error = EndpointExplorationError::InvalidDpuRedfishBiosResponse {
                    details,
                    response_body: None,
                    response_code: None,
                };
                let schema = exploration_error.operator_error_schema();
                tracing::warn!(
                    error = %error,
                    error_code = %schema.error_code,
                    mitigation = %schema.mitigation_for_log(),
                    text = %schema.text,
                    "Failed to fetch machine setup status"
                );
                (None, Some(exploration_error))
            }
            Err(error) => {
                let schema = OperatorErrorSchema::new(
                    ErrorCode::nico(ErrorSubsystem::SiteExplorer, 130),
                    format!("Failed to fetch machine setup status: {error}"),
                    None,
                );
                tracing::warn!(
                    error = %error,
                    error_code = %schema.error_code,
                    mitigation = %schema.mitigation_for_log(),
                    text = %schema.text,
                    "Failed to fetch machine setup status"
                );
                (None, None)
            }
        };

        let secure_boot_status = fetch_secure_boot_status(client.as_ref())
            .await
            .inspect_err(
                |error| tracing::warn!(%error, "Failed to fetch forge secure boot status."),
            )
            .ok();

        let lockdown_status = fetch_lockdown_status(client.as_ref())
            .await
            .inspect_err(|error| {
                if !matches!(error, libredfish::RedfishError::NotSupported(_)) {
                    tracing::warn!(%error, "Failed to fetch lockdown status.");
                }
            })
            .ok();

        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            machine_id: None,
            managers: vec![manager],
            systems: vec![system],
            chassis,
            service,
            vendor,
            versions: HashMap::default(),
            model: None,
            power_shelf_id: None,
            switch_id: None,
            machine_setup_status,
            secure_boot_status,
            lockdown_status,
            physical_slot_number: None,
            compute_tray_index: None,
            topology_id: None,
            revision_id: None,
            remediation_error,
        })
    }

    pub(super) async fn nv_generate_exploration_report(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        let service_root = self
            .nv_redfish_client_pool
            .service_root_with_cache_predicate(bmc_ip_address, credentials, |root| {
                let complete = root.root.chassis.is_some() && root.root.managers.is_some();
                if !complete {
                    tracing::warn!(
                        %bmc_ip_address,
                        chassis = root.root.chassis.is_some(),
                        managers = root.root.managers.is_some(),
                        "BMC served a service root without required navigation not caching it"
                    );
                }
                complete
            })
            .await
            .map_err(|err| EndpointExplorationError::Other {
                details: format!("Cannot Redfish service root: {err}"),
            })?;

        let mut report = bmc_explorer::nv_generate_exploration_report(
            service_root,
            &nv_bmc_explore_config(boot_interface),
        )
        .await
        .map_err(map_nv_redfish_explore_error)?;

        record_evaluated_boot_interface(report.machine_setup_status.as_mut(), boot_interface);

        Ok(report)
    }

    pub(super) async fn reset_bmc(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        reset_type: Option<libredfish::ManagerResetType>,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client
            .bmc_reset(reset_type)
            .await
            .map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn get_power_state(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<libredfish::PowerState, EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.get_power_state().await.map_err(map_redfish_error)
    }

    pub(super) async fn power(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        action: libredfish::SystemPowerControl,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.power(action).await.map_err(map_redfish_error)?;
        Ok(())
    }

    pub(super) async fn chassis_reset(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        chassis_id: &str,
        action: libredfish::SystemPowerControl,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client
            .chassis_reset(chassis_id, action)
            .await
            .map_err(map_redfish_error)?;
        Ok(())
    }

    pub(super) async fn disable_secure_boot(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client
            .disable_secure_boot()
            .await
            .map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn lockdown(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        action: libredfish::EnabledDisabled,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.lockdown(action).await.map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn lockdown_status(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<LockdownStatus, EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        let response = fetch_lockdown_status(client.as_ref())
            .await
            .map_err(map_redfish_error)?;

        Ok(response)
    }

    pub(super) async fn enable_infinite_boot(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client
            .enable_infinite_boot()
            .await
            .map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn is_infinite_boot_enabled(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<Option<bool>, EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client
            .is_infinite_boot_enabled()
            .await
            .map_err(map_redfish_error)
    }

    pub(super) async fn machine_setup(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        // We will be redoing machine_setup later and can worry about getting the profile right then.
        // Keep `empty_profiles` outside the closure so the returned future can borrow it.
        let empty_profiles: libredfish::BiosProfileVendor = HashMap::default();
        let result = match boot_interface {
            Some(target) => {
                target
                    .run(|bi| {
                        client.machine_setup(
                            Some(bi),
                            &empty_profiles,
                            libredfish::BiosProfileType::Performance,
                            &empty_profiles,
                        )
                    })
                    .await
            }
            None => {
                client
                    .machine_setup(
                        None,
                        &empty_profiles,
                        libredfish::BiosProfileType::Performance,
                        &empty_profiles,
                    )
                    .await
            }
        };
        result.map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn set_boot_order_dpu_first(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface: &BootInterfaceTarget,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        boot_interface
            .run(|bi| client.set_boot_order_dpu_first(bi))
            .await
            .map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn set_nic_mode(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        mode: NicMode,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.set_nic_mode(mode).await.map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn is_viking(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<bool, EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        let service_root = client.get_service_root().await.map_err(map_redfish_error)?;
        let system = client.get_system().await.map_err(map_redfish_error)?;
        let manager = client.get_manager().await.map_err(map_redfish_error)?;
        Ok(
            service_root.vendor().unwrap_or(RedfishVendor::Unknown) == RedfishVendor::AMI
                && system.id == "DGX"
                && manager.id == "BMC",
        )
    }

    pub(super) async fn clear_nvram(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.clear_nvram().await.map_err(map_redfish_error)?;
        Ok(())
    }

    pub(super) async fn create_bmc_user(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        new_username: &str,
        new_password: &str,
        new_user_role_id: libredfish::RoleId,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client
            .create_user(new_username, new_password, new_user_role_id)
            .await
            .map_err(map_redfish_error)?;
        Ok(())
    }

    pub(super) async fn delete_bmc_user(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        delete_user: &str,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(bmc_ip_address, credentials)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client
            .delete_user(delete_user)
            .await
            .map_err(map_redfish_error)?;
        Ok(())
    }

    pub(super) async fn probe_vendor_name_from_chassis(
        &self,
        bmc_ip_address: SocketAddr,
        username: String,
        password: String,
    ) -> Result<String, EndpointExplorationError> {
        let client = self
            .create_authenticated_redfish_client(
                bmc_ip_address,
                Credentials::UsernamePassword { username, password },
            )
            .await
            .map_err(map_redfish_client_creation_error)?;

        let chassis_ids = client.get_chassis_all().await.map_err(map_redfish_error)?;
        for chassis_id in &chassis_ids {
            let chassis = client
                .get_chassis(chassis_id)
                .await
                .map_err(map_redfish_error)?;
            if let Some(manufacturer) = chassis.manufacturer {
                return Ok(manufacturer);
            }
        }

        Err(EndpointExplorationError::UnsupportedVendor {
            vendor: "Unknown".to_string(),
        })
    }
}

async fn is_switch(client: &dyn Redfish) -> Result<bool, RedfishError> {
    let chassis = client.get_chassis_all().await?;
    Ok(chassis.contains(&"MGX_NVSwitch_0".to_string()))
}

async fn is_powershelf(client: &dyn Redfish) -> Result<bool, RedfishError> {
    let chassis_ids = client.get_chassis_all().await?;
    for chassis_id in &chassis_ids {
        if chassis_id == "powershelf" {
            return Ok(true);
        }
        if let Ok(chassis) = client.get_chassis(chassis_id).await
            && chassis.manufacturer.as_ref().is_some_and(|m| {
                let m = m.to_lowercase();
                m.contains("lite-on") || m.contains("delta")
            })
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn fetch_manager(client: &dyn Redfish) -> Result<Manager, RedfishError> {
    let manager = client.get_manager().await?;
    let ethernet_interfaces = fetch_ethernet_interfaces(client, false, false)
        .await
        .or_else(|err| match err {
            RedfishError::NotSupported(_) => Ok(vec![]),
            _ => Err(err),
        })?;

    // Warn if the manager eth0 MAC is locally-administered: a real BMC MAC is
    // globally unique, so this signals transient pre-sync data (seen briefly
    // after a BMC reboot) that would poison anything keyed on the BMC MAC.
    if let Some(eth0) = ethernet_interfaces.iter().find(|e| {
        e.id.as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case("eth0"))
    }) && let Some(mac) = eth0.mac_address
        && crate::is_locally_administered_mac(mac)
    {
        tracing::warn!(
            manager_id = %manager.id,
            eth0_mac_address = %mac,
            "manager eth0 MAC is locally-administered (transient pre-sync data?)",
        );
    }

    Ok(Manager {
        ethernet_interfaces,
        id: manager.id,
        ipmi_port: None,
    })
}

struct FetchedSystem {
    system: ComputerSystem,
    is_dpu: bool,
    is_host: bool,
    linked_chassis_ids: Vec<String>,
}

async fn fetch_system(client: &dyn Redfish) -> Result<FetchedSystem, EndpointExplorationError> {
    let mut system = client.get_system().await.map_err(map_redfish_error)?;
    let linked_chassis_ids = system
        .links
        .as_ref()
        .and_then(|links| links.chassis.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|chassis| {
            chassis
                .odata_id_get()
                .ok()
                .and_then(|id| id.rsplit('/').next())
                .map(str::to_owned)
        })
        .collect();
    let is_dpu = system.id.to_lowercase().contains("bluefield");
    let ethernet_interfaces = match fetch_ethernet_interfaces(client, true, is_dpu).await {
        Ok(interfaces) => Ok(interfaces),
        Err(e) if is_dpu => {
            tracing::warn!(
                error = %e,
                "Failed to get system Ethernet interfaces; ignoring the error"
            );
            Ok(Vec::default())
        }
        Err(e) => Err(map_redfish_error(e)),
    }?;
    let mut base_mac = None;
    let mut nic_mode = None;

    let is_switch = is_switch(client).await.map_err(map_redfish_error)?;
    let is_powershelf = is_powershelf(client).await.map_err(map_redfish_error)?;
    if is_dpu {
        // This part processes dpu case and do two things such as
        // 1. update system serial_number in case it is empty using chassis serial_number
        // 2. format serial_number data using the same rules as in fetch_chassis()
        if system.serial_number.is_none() {
            let chassis = client
                .get_chassis("Card1")
                .await
                .map_err(map_redfish_error)?;
            system.serial_number = chassis.serial_number;
        }

        base_mac = match client.get_base_mac_address().await {
            Ok(base_mac) => base_mac.and_then(|v| {
                v.parse()
                    .inspect_err(|err| {
                        tracing::warn!(
                            error = %err,
                            mac_address = %v,
                            "Failed to parse BaseMAC"
                        );
                    })
                    .ok()
            }),
            Err(error) => {
                tracing::info!(
                    serial_number = ?system.serial_number,
                    %error,
                    "Could not use new method to retrieve base MAC address for DPU"
                );
                None
            }
        };
        if base_mac.is_none() {
            // BF4 temporary patch:
            // BF4 BMC reports do not expose PF0 base MAC via the usual
            // ComputerSystem BaseMAC path, so we patch `systems[].base_mac` by
            // reading NDF0 PermanentMACAddress from the BF4 NIC subtree and
            // deriving base MAC as (NDF0 - 0x10).
            //
            // Remove this fallback once BF4 BMC exposes PF0 base MAC directly in
            // the standard system/base_mac report path.
            //
            // This path depends on NIC inventory being up and queryable; it may
            // be absent when NIC firmware is in recovery/uninitialized states or
            // when NIC-side inventory endpoints are not populated/responding.
            base_mac = get_base_mac_from_bf4_ndf0(client).await.map(Into::into);
            if base_mac.is_none() {
                tracing::warn!(
                    "BF4 NDF0 fallback did not provide PF0 base MAC (NIC inventory unavailable/uninitialized?)"
                );
            }
        }
        nic_mode = match client.get_nic_mode().await {
            Ok(nic_mode) => nic_mode,
            Err(e) => return Err(map_redfish_error(e)),
        };
    }

    system.serial_number = system.serial_number.map(|s| s.trim().to_string());

    let pcie_devices = if !is_powershelf {
        fetch_pcie_devices(client)
            .await
            .map_err(map_redfish_error)?
    } else {
        vec![]
    };

    let is_infinite_boot_enabled = client
        .is_infinite_boot_enabled()
        .await
        .map_err(map_redfish_error)?;

    // If this is an nvswitch, don't set a boot order.
    let boot_order = match is_switch || is_powershelf {
        true => {
            tracing::debug!("Skipping boot order for nvswitch or powershelf");
            None
        }
        false => fetch_boot_order(client, &system)
            .await
            .inspect_err(|error| tracing::warn!(%error, "Failed to fetch boot order."))
            .ok(),
    };

    let bios_version = system
        .bios_version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(str::to_string);

    Ok(FetchedSystem {
        system: ComputerSystem {
            ethernet_interfaces,
            id: system.id,
            manufacturer: system.manufacturer,
            model: system.model,
            serial_number: system.serial_number,
            attributes: ComputerSystemAttributes {
                nic_mode: nic_mode.map(IntoModel::into_model),
                is_infinite_boot_enabled,
            },
            pcie_devices,
            base_mac,
            power_state: system.power_state.into_model(),
            sku: system.sku,
            boot_order,
            bios_version,
            serial_console_ssh_port: None,
        },
        is_dpu,
        is_host: !(is_dpu || is_switch || is_powershelf),
        linked_chassis_ids,
    })
}

async fn fetch_ethernet_interfaces(
    client: &dyn Redfish,
    fetch_system_interfaces: bool,
    fetch_bluefield_oob: bool,
) -> Result<Vec<EthernetInterface>, RedfishError> {
    let eth_if_ids: Vec<String> = match match fetch_system_interfaces {
        false => client.get_manager_ethernet_interfaces().await,
        true => client.get_system_ethernet_interfaces().await,
    } {
        Ok(ids) => ids,
        Err(e) => {
            match e {
                RedfishError::HTTPErrorCode { status_code, .. } if status_code == NOT_FOUND => {
                    // missing oob for DPUs is handled below
                    Vec::new()
                }
                _ => return Err(e),
            }
        }
    };
    let mut eth_ifs: Vec<EthernetInterface> = Vec::new();
    let mut oob_found = false;

    for iface_id in eth_if_ids.iter() {
        let iface = match fetch_system_interfaces {
            false => client.get_manager_ethernet_interface(iface_id).await,
            true => client.get_system_ethernet_interface(iface_id).await,
        }?;

        oob_found |= iface_id.to_lowercase().contains("oob");

        let mac_address = if let Some(iface_mac_address) = iface.mac_address {
            match deserialize_input_mac_to_address(&iface_mac_address).map_err(|e| {
                RedfishError::GenericError {
                    error: format!("MAC address not valid: {iface_mac_address} (err: {e})"),
                }
            }) {
                Ok(mac) => Ok(Some(mac)),
                Err(e) => {
                    if iface
                        .interface_enabled
                        .is_some_and(|is_enabled| !is_enabled)
                    {
                        // disabled interfaces sometimes populate the MAC address with junk,
                        // ignore this error and create the interface with an empty mac address
                        // in the exploration report
                        tracing::debug!(
                            interface_id = %iface_id,
                            link_status = ?iface.link_status,
                            error = %e,
                            "could not parse MAC address for a disabled interface"
                        );
                        Ok(None)
                    } else {
                        Err(e)
                    }
                }
            }
        } else {
            Ok(None)
        }?;

        let uefi_device_path = if let Some(uefi_device_path) = iface.uefi_device_path {
            let path_as_version_string = UefiDevicePath::from_str(&uefi_device_path)
                .map_err(|error| RedfishError::GenericError { error })?;
            Some(path_as_version_string)
        } else {
            None
        };

        let iface = EthernetInterface {
            description: iface.description,
            id: iface.id,
            interface_enabled: iface.interface_enabled,
            mac_address,
            link_status: iface.link_status.map(|s| s.to_string()),
            uefi_device_path,
        };

        eth_ifs.push(iface);
    }

    if !oob_found && fetch_bluefield_oob {
        // Temporary workaround untill get_system_ethernet_interface will return oob interface information
        // Usually the workaround for not even being able to enumerate the interfaces
        // would be used. But if a future Bluefield BMC revision returns interfaces
        // but still misses the OOB interface, we would use this path.
        if let Some(oob_iface) = get_oob_interface(client).await? {
            eth_ifs.push(oob_iface);
        } else {
            return Err(RedfishError::GenericError {
                error: "oob interface missing for dpu".to_string(),
            });
        }
    }

    Ok(eth_ifs)
}

async fn get_oob_interface(
    client: &dyn Redfish,
) -> Result<Option<EthernetInterface>, RedfishError> {
    // If chassis.contains(&"MGX_NVSwitch_0".to_string()),
    // nvlink switch does not have oob interface. And, if we try
    // querying boot options over redfish, we will get a 404 error.
    // So just return Ok(None) here.
    if is_switch(client).await? || is_powershelf(client).await? {
        return Ok(None);
    }

    // Temporary workaround until oob mac would be possible to get via Redfish
    let boot_options = client.get_boot_options().await?;
    let mac_pattern = Regex::new(r"MAC\((?<mac>[[:alnum:]]+)\,").unwrap();
    let mut boot_order_first_ethernet_interface = None;

    for option in boot_options.members.iter() {
        // odata_id: "/redfish/v1/Systems/Bluefield/BootOptions/Boot0001"
        let option_id = option.odata_id.split('/').next_back().unwrap();
        let boot_option = client.get_boot_option(option_id).await?;
        // display_name: "NET-OOB-IPV4"
        if boot_option.display_name.contains("OOB") {
            if boot_option.uefi_device_path.is_none() {
                // Try whether there might be other matching options
                continue;
            }
            // UefiDevicePath: "MAC(B83FD2909582,0x1)/IPv4(0.0.0.0,0x0,DHCP,0.0.0.0,0.0.0.0,0.0.0.0)/Uri()"
            if let Some(captures) =
                mac_pattern.captures(boot_option.uefi_device_path.unwrap().as_str())
            {
                let mac_addr_str = captures.name("mac").unwrap().as_str();
                let mut mac_addr_builder = String::new();

                // Transform B83FD2909582 -> B8:3F:D2:90:95:82
                for (i, c) in mac_addr_str.chars().enumerate() {
                    mac_addr_builder.push(c);
                    if ((i + 1) % 2 == 0) && ((i + 1) < mac_addr_str.len()) {
                        mac_addr_builder.push(':');
                    }
                }

                let mac_addr =
                    deserialize_input_mac_to_address(&mac_addr_builder).map_err(|e| {
                        RedfishError::GenericError {
                            error: format!("MAC address not valid: {mac_addr_builder} (err: {e})"),
                        }
                    })?;

                let (description, id) = if boot_option.display_name.contains("OOB") {
                    (
                        Some("1G DPU OOB network interface".to_string()),
                        Some("oob_net0".to_string()),
                    )
                } else {
                    (boot_option.description, Some(option_id.to_string()))
                };

                boot_order_first_ethernet_interface = Some(EthernetInterface {
                    description: description.clone(),
                    id: id.clone(),
                    interface_enabled: None,
                    mac_address: Some(mac_addr),
                    link_status: None,
                    uefi_device_path: None,
                });
            }
        }
    }

    Ok(boot_order_first_ethernet_interface)
}

struct FetchedChassis {
    chassis: Vec<Chassis>,
}

fn should_fetch_network_adapter_ports(
    supports_adapter_port_mac_inventory: bool,
    is_host: bool,
    linked_chassis_ids: &[String],
) -> bool {
    supports_adapter_port_mac_inventory && is_host && !linked_chassis_ids.is_empty()
}

async fn fetch_network_adapter_port_mac_addresses(
    client: &dyn Redfish,
    chassis_id: &str,
    network_adapter_id: &str,
) -> Vec<MacAddress> {
    let port_ids = match client.get_ports(chassis_id, network_adapter_id).await {
        Ok(port_ids) => port_ids,
        Err(error) => {
            tracing::warn!(
                %chassis_id,
                %network_adapter_id,
                %error,
                "Failed to enumerate network adapter ports; continuing without port MAC addresses"
            );
            return Vec::new();
        }
    };

    let mut result = Vec::new();
    for port_id in port_ids {
        let port = match client
            .get_port(chassis_id, network_adapter_id, &port_id)
            .await
        {
            Ok(port) => port,
            Err(error) => {
                tracing::warn!(
                    %chassis_id,
                    %network_adapter_id,
                    %port_id,
                    %error,
                    "Failed to read network adapter port; continuing without its MAC addresses"
                );
                continue;
            }
        };
        let mac_addresses = match port.mac_addresses() {
            Ok(mac_addresses) => mac_addresses,
            Err(error) => {
                tracing::warn!(
                    %chassis_id,
                    %network_adapter_id,
                    %port_id,
                    %error,
                    "Failed to parse network adapter port MAC addresses; continuing without them"
                );
                continue;
            }
        };
        for mac_address in mac_addresses {
            if !result.contains(&mac_address) {
                result.push(mac_address);
            }
        }
    }
    result
}

async fn fetch_chassis(
    client: &dyn Redfish,
    port_chassis_ids: Option<&[String]>,
) -> Result<FetchedChassis, RedfishError> {
    let mut chassis: Vec<Chassis> = Vec::new();

    let chassis_list = client.get_chassis_all().await?;
    for chassis_id in &chassis_list {
        let Ok(desc) = client.get_chassis(chassis_id).await else {
            continue;
        };

        let net_adapter_list = if desc.network_adapters.is_some() {
            match client.get_chassis_network_adapters(chassis_id).await {
                Ok(v) => v,
                Err(RedfishError::NotSupported(_)) => vec![],
                // Nautobot uses Chassis_0 as the source of truth for the GB200 chassis serial number.
                // Other chassis subsystems with network adapters may report different serial numbers.
                Err(RedfishError::MissingKey { .. }) if chassis_id == "Chassis_0" => vec![],
                Err(_) => continue,
            }
        } else {
            vec![]
        };

        let mut net_adapters: Vec<NetworkAdapter> = Vec::new();
        for net_adapter_id in &net_adapter_list {
            let value = client
                .get_chassis_network_adapter(chassis_id, net_adapter_id)
                .await?;

            let port_mac_addresses = if value.ports.is_some()
                && port_chassis_ids.is_some_and(|chassis_ids| chassis_ids.contains(chassis_id))
            {
                fetch_network_adapter_port_mac_addresses(client, chassis_id, net_adapter_id).await
            } else {
                Vec::new()
            };

            let net_adapter = NetworkAdapter {
                id: value.id,
                manufacturer: value.manufacturer,
                model: value.model,
                part_number: value.part_number,
                serial_number: Some(
                    value
                        .serial_number
                        .as_ref()
                        .unwrap_or(&"".to_string())
                        .trim()
                        .to_string(),
                ),
                port_mac_addresses,
            };

            net_adapters.push(net_adapter);
        }

        // For GB200 and Vera Rubin hosts, use the Chassis_0 assembly serial number to
        // match Nautobot / expected-machine inventory serials.
        let serial_number = if chassis_id == "Chassis_0" {
            client
                .get_chassis_assembly("Chassis_0")
                .await
                .ok()
                .and_then(|assembly| {
                    assembly
                        .assemblies
                        .iter()
                        .find(|asm| {
                            let model = asm.model.as_deref();
                            model.is_some_and(
                                bmc_explorer::hw::vera_rubin::chassis_assembly_serial_model,
                            ) || model == Some("GB200 NVL")
                        })
                        .and_then(|asm| asm.serial_number.clone())
                })
                .or(desc.serial_number)
        } else {
            desc.serial_number
        };

        let nvidia_oem = desc.oem.as_ref().and_then(|x| x.nvidia.as_ref());
        chassis.push(Chassis {
            id: chassis_id.to_string(),
            manufacturer: desc.manufacturer,
            model: desc.model,
            part_number: desc.part_number,
            serial_number,
            network_adapters: net_adapters,
            physical_slot_number: nvidia_oem.and_then(|x| x.chassis_physical_slot_number),
            compute_tray_index: nvidia_oem.and_then(|x| x.compute_tray_index),
            topology_id: nvidia_oem.and_then(|x| x.topology_id),
            revision_id: nvidia_oem.and_then(|x| x.revision_id),
        });
    }

    Ok(FetchedChassis { chassis })
}

async fn get_base_mac_from_bf4_ndf0(client: &dyn Redfish) -> Option<MacAddress> {
    let ndf0_paths = [
        "/redfish/v1/Chassis/BlueField_0/NetworkAdapters/BlueField_NIC_0/NetworkDeviceFunctions/0",
        "/redfish/v1/Chassis/Card1/NetworkAdapters/Bluefield_NIC/NetworkDeviceFunctions/0",
    ];
    for path in ndf0_paths {
        let resource = match client.get_resource(ODataId::from(path)).await {
            Ok(resource) => resource,
            Err(RedfishError::NotSupported(_)) => continue,
            Err(_) => continue,
        };
        let body: serde_json::Value = match serde_json::from_str(resource.raw.get()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(mac) = body
            .pointer("/Ethernet/PermanentMACAddress")
            .and_then(serde_json::Value::as_str)
            && let Ok(parsed) = deserialize_input_mac_to_address(mac)
        {
            let derived = crate::mac_to_u64(parsed).checked_sub(BF4_NDF0_TO_BASE_MAC_OFFSET)?;
            return Some(crate::u64_to_mac(derived));
        }
    }
    None
}

async fn fetch_boot_order(
    client: &dyn Redfish,
    system: &libredfish::model::ComputerSystem,
) -> Result<BootOrder, RedfishError> {
    let boot_options_id =
        system
            .boot
            .boot_options
            .clone()
            .ok_or_else(|| RedfishError::MissingKey {
                key: "boot.boot_options".to_string(),
                url: system.odata.odata_id.to_string(),
            })?;

    let all_boot_options: Vec<libredfish::model::BootOption> = client
        .get_collection(boot_options_id)
        .await
        .and_then(|t1| t1.try_get::<libredfish::model::BootOption>())
        .into_iter()
        .flat_map(|x1| x1.members)
        .collect();

    let boot_order: Vec<BootOption> = system
        .boot
        .boot_order
        .iter()
        .filter_map(|ref_id| {
            all_boot_options
                .iter()
                .find(|opt| opt.boot_option_reference == *ref_id)
                .cloned()
                .map(IntoModel::into_model)
        })
        .collect();

    Ok(BootOrder { boot_order })
}

async fn fetch_pcie_devices(client: &dyn Redfish) -> Result<Vec<PCIeDevice>, RedfishError> {
    let pci_device_list = client.pcie_devices().await?;
    let mut pci_devices: Vec<PCIeDevice> = Vec::new();

    for pci_device in pci_device_list {
        pci_devices.push(PCIeDevice {
            description: pci_device.description,
            firmware_version: pci_device.firmware_version,
            id: pci_device.id.clone(),
            manufacturer: pci_device.manufacturer,
            gpu_vendor: pci_device.gpu_vendor,
            name: pci_device.name,
            part_number: pci_device.part_number,
            serial_number: pci_device.serial_number,
            status: pci_device.status.map(IntoModel::into_model),
        });
    }
    Ok(pci_devices)
}

async fn fetch_service(client: &dyn Redfish) -> Result<Vec<Service>, RedfishError> {
    let mut service: Vec<Service> = Vec::new();

    let inventory_list = client.get_software_inventories().await?;
    let mut inventories: Vec<Inventory> = Vec::new();
    for inventory_id in &inventory_list {
        let Ok(value) = client.get_firmware(inventory_id).await else {
            continue;
        };

        let inventory = Inventory {
            id: value.id,
            description: value.description,
            version: value.version,
            release_date: value.release_date,
        };

        inventories.push(inventory);
    }

    service.push(Service {
        id: "FirmwareInventory".to_string(),
        inventories,
    });

    Ok(service)
}

async fn fetch_machine_setup_status(
    client: &dyn Redfish,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<MachineSetupStatus, RedfishError> {
    let status = match boot_interface {
        Some(target) => {
            target
                .run(|boot_interface| client.machine_setup_status(Some(boot_interface)))
                .await?
        }
        None => client.machine_setup_status(None).await?,
    };
    let mut diffs: Vec<MachineSetupDiff> = Vec::new();

    for diff in status.diffs {
        diffs.push(MachineSetupDiff {
            key: diff.key,
            expected: diff.expected,
            actual: diff.actual,
        });
    }

    Ok(MachineSetupStatus {
        is_done: status.is_done,
        diffs,
        evaluated_boot_interface: boot_interface.map(MachineBootInterfaceTarget::from),
    })
}

async fn fetch_secure_boot_status(client: &dyn Redfish) -> Result<SecureBootStatus, RedfishError> {
    let status = client.get_secure_boot().await?;

    let secure_boot_enable =
        status
            .secure_boot_enable
            .ok_or_else(|| RedfishError::GenericError {
                error: "expected secure_boot_enable_field set in secure boot response".to_string(),
            })?;

    let secure_boot_current_boot =
        status
            .secure_boot_current_boot
            .ok_or_else(|| RedfishError::GenericError {
                error: "expected secure_boot_current_boot set in secure boot response".to_string(),
            })?;

    let is_enabled = secure_boot_enable && secure_boot_current_boot.is_enabled();

    Ok(SecureBootStatus { is_enabled })
}

async fn fetch_lockdown_status(client: &dyn Redfish) -> Result<LockdownStatus, RedfishError> {
    let status = client.lockdown_status().await?;
    let internal_status = if status.is_fully_enabled() {
        InternalLockdownStatus::Enabled
    } else if status.is_fully_disabled() {
        InternalLockdownStatus::Disabled
    } else {
        InternalLockdownStatus::Partial
    };
    Ok(LockdownStatus {
        status: internal_status,
        message: status.message().to_string(),
    })
}

pub(crate) fn map_redfish_client_creation_error(
    error: RedfishClientCreationError,
) -> EndpointExplorationError {
    match error {
        RedfishClientCreationError::MissingCredentials { key } => {
            EndpointExplorationError::MissingCredentials {
                key,
                cause: "credentials are missing in the secret engine".into(),
            }
        }
        RedfishClientCreationError::SecretEngineError { cause } => {
            EndpointExplorationError::SecretsEngineError {
                cause: format!("secret engine error occurred: {cause:#}"),
            }
        }
        RedfishClientCreationError::RedfishError(e) => map_redfish_error(e),
        RedfishClientCreationError::InvalidHeader(original_error) => {
            EndpointExplorationError::Other {
                details: format!("RedfishClientError::InvalidHeader: {original_error}"),
            }
        }
        RedfishClientCreationError::MissingArgument(argument) => EndpointExplorationError::Other {
            details: format!("Missing argument to RedFish client: {argument}"),
        },
    }
}

pub(crate) fn map_redfish_error(error: RedfishError) -> EndpointExplorationError {
    match &error {
        RedfishError::NetworkError { url, source } => {
            let details = format!("url: {url};\nsource: {source};\nerror: {error}");
            if source.is_connect() {
                EndpointExplorationError::ConnectionRefused { details }
            } else if source.is_timeout() {
                EndpointExplorationError::ConnectionTimeout { details }
            } else {
                EndpointExplorationError::Unreachable {
                    details: Some(details),
                }
            }
        }
        RedfishError::HTTPErrorCode {
            status_code,
            response_body,
            url,
        } if *status_code == http::StatusCode::FORBIDDEN && url.contains("FirmwareInventory") => {
            EndpointExplorationError::VikingFWInventoryForbiddenError {
                details: format!(
                    "HTTP {status_code} at {url} - this is a known, intermittent issue for DGX H100 BMCs."
                ),
                response_body: Some(response_body.clone()),
                response_code: Some(status_code.as_u16()),
            }
        }
        RedfishError::HTTPErrorCode {
            status_code,
            response_body,
            url,
        } if *status_code == http::StatusCode::UNAUTHORIZED
            || *status_code == http::StatusCode::FORBIDDEN =>
        {
            let code_str = status_code.as_str();
            EndpointExplorationError::Unauthorized {
                details: format!("HTTP {status_code} {code_str} at {url}"),
                response_body: Some(response_body.clone()),
                response_code: Some(status_code.as_u16()),
            }
        }
        RedfishError::HTTPErrorCode {
            status_code,
            response_body,
            url,
        } => EndpointExplorationError::RedfishError {
            details: format!("HTTP {status_code} at {url}"),
            response_body: Some(response_body.clone()),
            response_code: Some(status_code.as_u16()),
        },
        RedfishError::JsonDeserializeError { url, body, source } => {
            EndpointExplorationError::RedfishError {
                details: format!("Failed to deserialize data from {url}: {source}"),
                response_body: Some(body.clone()),
                response_code: None,
            }
        }
        _ => EndpointExplorationError::RedfishError {
            details: error.to_string(),
            response_body: None,
            response_code: None,
        },
    }
}

fn nv_error_classifier(
    err: &carbide_redfish::nv_redfish::BmcError,
) -> Option<bmc_explorer::ErrorClass> {
    type BmcError = carbide_redfish::nv_redfish::BmcError;
    match err {
        BmcError::InvalidResponse { status, .. } => match *status {
            http::StatusCode::NOT_FOUND => Some(bmc_explorer::ErrorClass::NotFound),
            http::StatusCode::INTERNAL_SERVER_ERROR => {
                Some(bmc_explorer::ErrorClass::InternalServerError)
            }
            _ => None,
        },
        _ => None,
    }
}

fn nv_bmc_explore_config(
    boot_interface: Option<&BootInterfaceTarget>,
) -> bmc_explorer::Config<'static, carbide_redfish::nv_redfish::RedfishBmc> {
    bmc_explorer::Config {
        boot_interface_mac: boot_interface.map(BootInterfaceTarget::mac_address),
        error_classifier: &nv_error_classifier,
        // Chosen arbitrarily: we want to wait a bit between tries,
        // but not for too long relative to the total exploration
        // time.
        retry_timeout: Duration::from_millis(1000),
    }
}

fn record_evaluated_boot_interface(
    machine_setup_status: Option<&mut MachineSetupStatus>,
    boot_interface: Option<&BootInterfaceTarget>,
) {
    if let Some(status) = machine_setup_status {
        // NvRedfish currently matches by MAC, but the observation remains
        // correlated with the complete logical target requested by NICo.
        status.evaluated_boot_interface = boot_interface.map(MachineBootInterfaceTarget::from);
    }
}

fn map_nv_redfish_explore_error(
    err: bmc_explorer::Error<carbide_redfish::nv_redfish::RedfishBmc>,
) -> EndpointExplorationError {
    type BmcError = carbide_redfish::nv_redfish::BmcError;
    use carbide_redfish::nv_redfish::Error;
    match err {
        bmc_explorer::Error::NvRedfish { context, err } => match err {
            Error::Bmc(err) => match err {
                BmcError::ReqwestError(err) => {
                    let details = format!(
                        "context: {context}; network error: {err}; source: {:?}",
                        err.source()
                    );
                    if err.is_connect() {
                        EndpointExplorationError::ConnectionRefused { details }
                    } else if err.is_timeout() {
                        EndpointExplorationError::ConnectionTimeout { details }
                    } else {
                        EndpointExplorationError::Unreachable {
                            details: Some(details),
                        }
                    }
                }
                BmcError::InvalidResponse { url, status, text } => {
                    match status {
                        // Disclaimer: this is original libredfish code...
                        http::StatusCode::FORBIDDEN
                            if url.to_string().contains("FirmwareInventory") =>
                        {
                            EndpointExplorationError::VikingFWInventoryForbiddenError {
                                details: format!(
                                    "HTTP {status} at {url} - this is a known, intermittent issue for DGX H100 BMCs."
                                ),
                                response_body: Some(text),
                                response_code: Some(status.as_u16()),
                            }
                        }
                        http::StatusCode::UNAUTHORIZED | http::StatusCode::FORBIDDEN => {
                            EndpointExplorationError::Unauthorized {
                                details: format!(
                                    "HTTP {status} {} at {context} ({url})",
                                    status.as_str()
                                ),
                                response_body: Some(text),
                                response_code: Some(status.as_u16()),
                            }
                        }
                        _ => EndpointExplorationError::RedfishError {
                            details: format!("HTTP {status} at {context} ({url})"),
                            response_body: Some(text),
                            response_code: Some(status.as_u16()),
                        },
                    }
                }
                BmcError::JsonError(err) => EndpointExplorationError::RedfishError {
                    details: format!("context: {context}; json error: {err}"),
                    response_body: None,
                    response_code: None,
                },
                err => EndpointExplorationError::RedfishError {
                    details: format!("context: {context}; error: {err}"),
                    response_body: None,
                    response_code: None,
                },
            },
            Error::Json(err) => EndpointExplorationError::RedfishError {
                details: format!("context: {context}; json error: {err}"),
                response_body: None,
                response_code: None,
            },
            err => EndpointExplorationError::RedfishError {
                details: format!("context: {context}; error: {err}"),
                response_body: None,
                response_code: None,
            },
        },
        err => EndpointExplorationError::Other {
            details: err.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    use arc_swap::ArcSwap;
    use carbide_redfish::libredfish::test_support::{RedfishSim, RedfishSimBootInterfaceRef};
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};
    use carbide_redfish::nv_redfish::NvRedfishClientPool;
    use carbide_secrets::credentials::Credentials;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases_async, check_values, value_scenarios};
    use libredfish::model::service_root::RedfishVendor;
    use mac_address::MacAddress;
    use model::machine_boot_interface::{MachineBootInterface, MachineBootInterfaceTarget};

    use super::{
        BootInterfaceTarget, EndpointExplorationError, MachineSetupStatus, RedfishClient,
        fetch_machine_setup_status, nv_bmc_explore_config, record_evaluated_boot_interface,
        should_fetch_network_adapter_ports,
    };

    fn test_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 443)
    }

    fn build_redfish_client(sim: Arc<RedfishSim>) -> RedfishClient {
        let proxy_address = Arc::new(ArcSwap::new(Arc::new(None)));
        let nv_pool = Arc::new(NvRedfishClientPool::new(proxy_address));
        RedfishClient::new(sim, nv_pool)
    }

    #[tokio::test]
    async fn detected_lenovo_host_collects_linked_network_adapter_port_mac() {
        let mac_address = MacAddress::new([0x94, 0x6d, 0xae, 0x53, 0xcb, 0x9b]);
        let sim = Arc::new(RedfishSim::default());
        sim.set_service_root_vendor(Some("Lenovo".to_string()));
        sim.set_system_id("System");
        sim.set_system_chassis_ids(vec!["Card1".to_string()]);
        sim.set_network_adapter_port_mac_addresses(vec![mac_address]);
        let redfish = build_redfish_client(sim);

        let report = redfish
            .generate_exploration_report(
                test_addr(),
                Credentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "password".to_string(),
                },
                None,
                None,
            )
            .await
            .unwrap();

        assert!(
            report.systems[0]
                .ethernet_interfaces
                .iter()
                .all(|interface| interface.mac_address.is_none()),
            "Port data must not become a System EthernetInterface",
        );
        let adapter = report
            .chassis
            .iter()
            .find(|chassis| chassis.id == "Card1")
            .and_then(|chassis| chassis.network_adapters.first())
            .expect("linked chassis network adapter");
        assert_eq!(adapter.port_mac_addresses, vec![mac_address]);
        assert_eq!(report.all_mac_addresses(), vec![mac_address]);
        assert_eq!(report.find_interface_id_for_mac(mac_address), None);
        assert_eq!(report.complete_boot_interfaces().count(), 0);
    }

    #[tokio::test]
    async fn network_adapter_port_invalid_ids_return_errors_without_poisoning_the_sim() {
        let sim = RedfishSim::default();
        sim.set_network_adapter_port_mac_addresses(vec![MacAddress::new([
            0x94, 0x6d, 0xae, 0x53, 0xcb, 0x9b,
        ])]);
        let client = sim
            .create_client("test-host", None, RedfishAuth::Anonymous, None)
            .await
            .unwrap();

        for (scenario, port_id) in [
            ("non-numeric identifier", "not-a-port"),
            ("out-of-range identifier", "1"),
        ] {
            assert!(
                client.get_port("Card1", "0", port_id).await.is_err(),
                "{scenario} should return an error",
            );
            assert!(
                client.get_port("Card1", "0", "0").await.is_ok(),
                "{scenario} should not poison simulator state",
            );
        }
    }

    #[test]
    fn network_adapter_port_inventory_gate_cases() {
        #[derive(Clone)]
        struct Input {
            supports_adapter_port_mac_inventory: bool,
            is_host: bool,
            linked_chassis_ids: Vec<String>,
        }

        check_values(
            [
                Check {
                    scenario: "eligible host uses linked adapter ports",
                    input: Input {
                        supports_adapter_port_mac_inventory: true,
                        is_host: true,
                        linked_chassis_ids: vec!["Card1".to_string()],
                    },
                    expect: true,
                },
                Check {
                    scenario: "non-host skips adapter ports",
                    input: Input {
                        supports_adapter_port_mac_inventory: true,
                        is_host: false,
                        linked_chassis_ids: vec!["Card1".to_string()],
                    },
                    expect: false,
                },
                Check {
                    scenario: "platform without adapter-port inventory skips adapter ports",
                    input: Input {
                        supports_adapter_port_mac_inventory: false,
                        is_host: true,
                        linked_chassis_ids: vec!["Card1".to_string()],
                    },
                    expect: false,
                },
                Check {
                    scenario: "eligible host without chassis links skips adapter ports",
                    input: Input {
                        supports_adapter_port_mac_inventory: true,
                        is_host: true,
                        linked_chassis_ids: vec![],
                    },
                    expect: false,
                },
            ],
            |input| {
                should_fetch_network_adapter_ports(
                    input.supports_adapter_port_mac_inventory,
                    input.is_host,
                    &input.linked_chassis_ids,
                )
            },
        );
    }

    async fn machine_setup_status_target(
        target: Option<BootInterfaceTarget>,
    ) -> Result<
        (
            Option<MachineBootInterfaceTarget>,
            Vec<Option<RedfishSimBootInterfaceRef>>,
        ),
        String,
    > {
        let sim = RedfishSim::default();
        let client = sim
            .create_client("test-host", None, RedfishAuth::Anonymous, None)
            .await
            .map_err(|error| error.to_string())?;
        let status = fetch_machine_setup_status(client.as_ref(), target.as_ref())
            .await
            .map_err(|error| error.to_string())?;

        Ok((
            status.evaluated_boot_interface,
            sim.machine_setup_status_targets("test-host"),
        ))
    }

    #[tokio::test]
    async fn machine_setup_status_records_the_exact_evaluated_target() {
        let mac_address = MacAddress::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01]);
        let boot_interface = MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.7-1-1".to_string(),
        };

        check_cases_async(
            [
                Case {
                    scenario: "complete pair",
                    input: Some(BootInterfaceTarget::Pair(boot_interface.clone())),
                    expect: Yields((
                        Some(MachineBootInterfaceTarget::Pair(boot_interface.clone())),
                        vec![Some(RedfishSimBootInterfaceRef::Pair {
                            mac_address,
                            interface_id: boot_interface.interface_id.clone(),
                        })],
                    )),
                },
                Case {
                    scenario: "legacy MAC only",
                    input: Some(BootInterfaceTarget::MacOnly(mac_address)),
                    expect: Yields((
                        Some(MachineBootInterfaceTarget::MacOnly(mac_address)),
                        vec![Some(RedfishSimBootInterfaceRef::Mac(mac_address))],
                    )),
                },
                Case {
                    scenario: "no boot interface",
                    input: None,
                    expect: Yields((None, vec![None])),
                },
            ],
            machine_setup_status_target,
        )
        .await;
    }

    #[test]
    fn nvredfish_projects_the_mac_and_records_the_logical_target() {
        let mac_address = MacAddress::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01]);
        let boot_interface = MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.7-1-1".to_string(),
        };

        value_scenarios!(run = |target: Option<BootInterfaceTarget>| {
            let config = nv_bmc_explore_config(target.as_ref());
            let mut status = MachineSetupStatus::default();
            record_evaluated_boot_interface(Some(&mut status), target.as_ref());
            (
                config.boot_interface_mac,
                status.evaluated_boot_interface,
            )
        };
            "complete pair" {
                Some(BootInterfaceTarget::Pair(boot_interface.clone())) => (
                    Some(mac_address),
                    Some(MachineBootInterfaceTarget::Pair(boot_interface)),
                ),
            }

            "legacy MAC only" {
                Some(BootInterfaceTarget::MacOnly(mac_address)) => (
                    Some(mac_address),
                    Some(MachineBootInterfaceTarget::MacOnly(mac_address)),
                ),
            }

            "no boot interface" {
                None => (None, None),
            }
        );
    }

    /// Rotate a BMC's root password against the sim and report the vendor
    /// each `create_client` call was made with, in order.
    ///
    /// This is what the password-rotation contract is asserted against: the
    /// FIRST client (which makes the actual `change_password_by_id` PATCH to
    /// `/AccountService`) must be uninitialized (`Some(RedfishVendor::Unknown)`),
    /// and only the SECOND client (which sets the password policy afterward)
    /// should carry the real `vendor`.
    async fn password_change_client_vendors(
        vendor: RedfishVendor,
    ) -> Result<Vec<Option<RedfishVendor>>, EndpointExplorationError> {
        let sim = Arc::new(RedfishSim::default());
        sim.seed_user("root", "factory_pass");

        let redfish = build_redfish_client(sim.clone());

        let factory_creds = Credentials::UsernamePassword {
            username: "root".to_string(),
            password: "factory_pass".to_string(),
        };

        redfish
            .set_bmc_root_password(test_addr(), vendor, factory_creds, "site_pass".to_string())
            .await?;

        Ok(sim
            .create_client_calls()
            .into_iter()
            .map(|call| call.vendor)
            .collect())
    }

    /// When site-explorer rotates a BMC's root password it must make the
    /// rotation PATCH with an uninitialized `Unknown` client and only use the
    /// real vendor on the follow-up policy client.
    ///
    /// The vendor-specific client triggers libredfish's full init path
    /// (fetches `/Systems`, `/Managers`, `/Chassis`) which is unnecessary just
    /// to PATCH `/AccountService`. Worse, factory BMCs like NVIDIA GBx00
    /// authenticate the supplied creds but return HTTP 403
    /// `Base.1.18.1.PasswordChangeRequired` on `/Systems` until the password is
    /// rotated -- so an init-first flow blocks the very PATCH that would unblock
    /// it. Only the SECOND client (set after the rotation succeeds) should be
    /// vendor-specific, so `set_machine_password_policy` gets the right impl
    /// (e.g. Lite-On omits `AccountLockoutCounterResetAfter`).
    ///
    /// Asserted as a table over vendors: each yields exactly two
    /// `create_client` calls -- always `Unknown` first, then the real vendor --
    /// which pins the call count, the uninitialized rotation client, and the
    /// vendor-specific policy client all at once.
    #[tokio::test]
    async fn set_bmc_root_password_rotates_with_unknown_then_real_vendor() {
        check_cases_async(
            [
                Case {
                    scenario: "Lite-On power shelf gets a vendor-specific policy client",
                    input: RedfishVendor::LiteOnPowerShelf,
                    expect: Yields(vec![
                        Some(RedfishVendor::Unknown),
                        Some(RedfishVendor::LiteOnPowerShelf),
                    ]),
                },
                Case {
                    scenario: "NVIDIA DPU gets a vendor-specific policy client too",
                    input: RedfishVendor::NvidiaDpu,
                    expect: Yields(vec![
                        Some(RedfishVendor::Unknown),
                        Some(RedfishVendor::NvidiaDpu),
                    ]),
                },
            ],
            password_change_client_vendors,
        )
        .await;
    }
}
