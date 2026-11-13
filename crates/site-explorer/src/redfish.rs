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
use std::sync::Arc;
use std::time::Duration;

use bmc_backend::BackendBmc;
use bmc_platform::{PlatformError, PowerState, ResetType};
use bmc_runtime::DriverConnector;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::{
    BmcCredentialOps, RedfishAuth, RedfishClientCreationError, RedfishClientPool,
};
use carbide_redfish::nv_redfish::{NvRedfishClientPool, RedfishBmc};
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use carbide_utils::redfish::BmcAccessInfo;
use libredfish::model::oem::nvidia_dpu::NicMode;
use libredfish::model::service_root::RedfishVendor;
use libredfish::{Redfish, RedfishError};
use mac_address::MacAddress;
use model::machine_boot_interface::MachineBootInterfaceTarget;
use model::site_explorer::{
    EndpointExplorationError, EndpointExplorationReport, InternalLockdownStatus, LockdownStatus,
    MachineSetupStatus,
};
use nv_redfish::computer_system::ComputerSystem;
use nv_redfish::manager::Manager;

// RedfishClient is a wrapper around a redfish client pool and implements redfish utility functions that the site explorer utilizes.
// TODO: In the future, we should refactor a lot of this client's work to api/src/redfish.rs because other components in carbide can utilize this functionality.
// Eventually, this file should only have code related to generating the site exploration report.
#[derive(Clone)]
pub(super) struct RedfishClient {
    redfish_client_pool: Arc<dyn BmcCredentialOps>,
    nv_redfish_client_pool: Arc<NvRedfishClientPool>,
    /// Pools targeting nico-bmc-proxy; `Some` only when `[bmc_proxy]` is
    /// enabled. Established-endpoint traffic uses them; everything else,
    /// and everything when `None`, uses the direct pools above exactly as
    /// before the proxy existed.
    proxied: Option<ProxiedPools>,
    /// Connects moved BMC operations through their drivers when the site
    /// sets `bmc_backend = "drivers"`; `None` runs them through libredfish.
    pub(super) driver_connector: Option<Arc<DriverConnector>>,
}

/// The pools that reach nico-bmc-proxy, handed to site-explorer only when
/// `[bmc_proxy]` is enabled.
#[derive(Clone)]
pub struct ProxiedPools {
    pub redfish: Arc<dyn RedfishClientPool>,
    pub nv_redfish: Arc<NvRedfishClientPool>,
}

/// An endpoint whose stored per-BMC root credential is established: the
/// caller already resolved it (and validated it is non-empty), and the
/// MAC names the same `BmcRoot{mac}` key nico-bmc-proxy resolves itself.
#[derive(Clone)]
pub struct EstablishedBmc {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) credentials: Credentials,
}

/// How site-explorer reaches a BMC for one operation.
///
/// `Established` is ordinary traffic: with `[bmc_proxy]` enabled it routes
/// via nico-bmc-proxy authenticating by key; otherwise it dials the BMC
/// directly with the carried credential, exactly as before. `Direct`
/// carries explicit credentials (factory defaults, expected-entity entries,
/// a just-set password) and always dials the BMC directly: this is the
/// credential-setup path.
#[derive(Clone)]
pub enum BmcAccess {
    Established(EstablishedBmc),
    Direct(Credentials),
}

impl RedfishClient {
    pub(super) fn new(
        redfish_client_pool: Arc<dyn BmcCredentialOps>,
        nv_redfish_client_pool: Arc<NvRedfishClientPool>,
        proxied: Option<ProxiedPools>,
    ) -> Self {
        Self {
            redfish_client_pool,
            nv_redfish_client_pool,
            proxied,
            driver_connector: None,
        }
    }

    /// Connects to an established endpoint through the configured
    /// `bmc_backend`, selecting drivers for the identity `report` recorded.
    /// The drivers backend fails without an explored report.
    pub(super) async fn connect_established(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
        report: Option<&EndpointExplorationReport>,
    ) -> Result<BackendBmc, EndpointExplorationError> {
        let Some(connector) = &self.driver_connector else {
            return self
                .create_established_redfish_client(bmc_ip_address, bmc, None)
                .await
                .map(BackendBmc::Libredfish)
                .map_err(map_redfish_client_creation_error);
        };
        let Some(report) = report else {
            return Err(EndpointExplorationError::Other {
                details: format!("BMC {bmc_ip_address} has no explored report to select drivers"),
            });
        };
        let bmc_access_info = BmcAccessInfo {
            host: bmc_ip_address.ip().to_string(),
            port: Some(bmc_ip_address.port()),
            mac_address: bmc.bmc_mac_address,
        };
        connector
            .connect(&bmc_access_info, report)
            .await
            .map(BackendBmc::from)
            .map_err(|error| EndpointExplorationError::Other {
                details: format!("Cannot connect to {bmc_ip_address} through its drivers: {error}"),
            })
    }

    /// Client for an established endpoint. With `[bmc_proxy]` enabled it
    /// authenticates by credential key on the proxied pool (the proxy
    /// resolves the same `BmcRoot{mac}` key); otherwise it is the plain
    /// direct client with the credential the caller already resolved.
    async fn create_established_redfish_client(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        match &self.proxied {
            Some(proxied) => {
                proxied
                    .redfish
                    .create_client(
                        &bmc_ip_address.ip().to_string(),
                        Some(bmc_ip_address.port()),
                        RedfishAuth::Key(CredentialKey::BmcCredentials {
                            credential_type: BmcCredentialType::BmcRoot {
                                bmc_mac_address: bmc.bmc_mac_address,
                            },
                        }),
                        vendor,
                    )
                    .await
            }
            None => {
                self.create_direct_redfish_client(bmc_ip_address, bmc.credentials, vendor)
                    .await
            }
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
                let observed = service_root.vendor_string();
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

    pub(super) async fn restore_bmc_root_ipmi_access(
        &self,
        bmc_ip_address: SocketAddr,
        vendor: RedfishVendor,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client_pool
            .restore_bmc_root_ipmi_access(
                &bmc_ip_address.ip().to_string(),
                Some(bmc_ip_address.port()),
                vendor,
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

    /// Picks the nv-redfish pool and credentials for one report fetch.
    ///
    /// Established with `[bmc_proxy]` enabled: the proxied pool with no
    /// credentials (the proxy resolves them). Established otherwise: the
    /// direct pool with the credential the caller already resolved --
    /// exactly the pre-split behavior. Direct: the caller's explicit
    /// credentials on the direct pool.
    fn nv_pool_and_credentials(
        &self,
        access: BmcAccess,
    ) -> (&NvRedfishClientPool, Option<Credentials>) {
        match (access, &self.proxied) {
            (BmcAccess::Established(_), Some(proxied)) => (proxied.nv_redfish.as_ref(), None),
            (BmcAccess::Established(bmc), None) => {
                (self.nv_redfish_client_pool.as_ref(), Some(bmc.credentials))
            }
            (BmcAccess::Direct(credentials), _) => {
                (self.nv_redfish_client_pool.as_ref(), Some(credentials))
            }
        }
    }

    pub(super) async fn nv_generate_exploration_report(
        &self,
        bmc_ip_address: SocketAddr,
        access: BmcAccess,
        boot_interface: Option<&BootInterfaceTarget>,
        explore_drive_firmware: bool,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        let (nv_pool, credentials) = self.nv_pool_and_credentials(access);
        let service_root = nv_pool
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
            &nv_bmc_explore_config(boot_interface, explore_drive_firmware),
        )
        .await
        .map_err(map_nv_redfish_explore_error)?;

        record_evaluated_boot_interface(report.machine_setup_status.as_mut(), boot_interface);

        Ok(report)
    }

    /// Restarts the BMC. Without `reset_type` the platform picks the reset its
    /// BMC needs; the drivers do so only for a BMC exploration identified, and
    /// a BMC they do not reset receives a plain Redfish `Manager.Reset`. A
    /// `reset_type` bypasses [`BackendBmc`], whose reset takes none.
    pub(super) async fn reset_bmc(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
        report: Option<&EndpointExplorationReport>,
        reset_type: Option<ResetType>,
    ) -> Result<(), EndpointExplorationError> {
        if let (Some(reset_type), None) = (reset_type, &self.driver_connector) {
            let reset_type = libredfish_manager_reset_type(reset_type)?;
            let client = self
                .create_established_redfish_client(bmc_ip_address, bmc, None)
                .await
                .map_err(map_redfish_client_creation_error)?;
            return client
                .bmc_reset(Some(reset_type))
                .await
                .map_err(map_redfish_error);
        }
        let report = identifying(report);
        if self.driver_connector.is_some() && (report.is_none() || reset_type.is_some()) {
            let manager = self.host_manager(bmc_ip_address, bmc).await?;
            return manager.reset(reset_type).await.map(drop).map_err(|err| {
                map_nv_redfish_explore_error(bmc_explorer::Error::NvRedfish {
                    context: "manager reset",
                    err,
                })
            });
        }
        let bmc = self
            .connect_established(bmc_ip_address, bmc, report)
            .await?;
        let bmc_control = bmc
            .bmc_control()
            .map_err(|error| map_platform_error(error.into()))?;
        bmc_control.reset().await.map_err(map_platform_error)?;
        Ok(())
    }

    pub(super) async fn get_power_state(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
        report: Option<&EndpointExplorationReport>,
    ) -> Result<Option<PowerState>, EndpointExplorationError> {
        let report = identifying(report);
        if self.driver_connector.is_some() && report.is_none() {
            let system = self.host_system(bmc_ip_address, bmc).await?;
            return Ok(system.power_state());
        }
        let bmc = self
            .connect_established(bmc_ip_address, bmc, report)
            .await?;
        let power = bmc
            .power()
            .map_err(|error| map_platform_error(error.into()))?;
        power.state().await.map_err(map_platform_error)
    }

    pub(super) async fn power(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
        report: Option<&EndpointExplorationReport>,
        action: ResetType,
    ) -> Result<(), EndpointExplorationError> {
        let report = identifying(report);
        if self.driver_connector.is_some() && report.is_none() {
            let system = self.host_system(bmc_ip_address, bmc).await?;
            return system.reset(Some(action)).await.map(drop).map_err(|err| {
                map_nv_redfish_explore_error(bmc_explorer::Error::NvRedfish {
                    context: "system reset",
                    err,
                })
            });
        }
        let bmc = self
            .connect_established(bmc_ip_address, bmc, report)
            .await?;
        let power = bmc
            .power()
            .map_err(|error| map_platform_error(error.into()))?;
        power.set(action).await.map_err(map_platform_error)?;
        Ok(())
    }

    pub(super) async fn chassis_reset(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
        report: Option<&EndpointExplorationReport>,
        chassis_id: &str,
        action: ResetType,
    ) -> Result<(), EndpointExplorationError> {
        let bmc = self
            .connect_established(bmc_ip_address, bmc, report)
            .await?;
        let power = bmc
            .power()
            .map_err(|error| map_platform_error(error.into()))?;
        power
            .chassis_reset(chassis_id, action)
            .await
            .map_err(map_platform_error)?;
        Ok(())
    }

    /// The host system, chosen as exploration chooses it, for power calls
    /// without an explored report to select drivers from.
    async fn host_system(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
    ) -> Result<ComputerSystem<RedfishBmc>, EndpointExplorationError> {
        let nv_error = |context| {
            move |err| map_nv_redfish_explore_error(bmc_explorer::Error::NvRedfish { context, err })
        };
        let (nv_pool, credentials) = self.nv_pool_and_credentials(BmcAccess::Established(bmc));
        let mut systems = nv_pool
            .service_root(bmc_ip_address, credentials)
            .await
            .map_err(|err| EndpointExplorationError::Other {
                details: format!("Cannot Redfish service root: {err}"),
            })?
            .systems()
            .await
            .map_err(nv_error("systems"))?
            .ok_or_else(|| EndpointExplorationError::Other {
                details: format!("BMC {bmc_ip_address} lists no computer systems"),
            })?
            .members()
            .await
            .map_err(nv_error("systems members"))?;
        let index = bmc_explorer::primary_system_index(&systems).ok_or_else(|| {
            EndpointExplorationError::Other {
                details: format!("BMC {bmc_ip_address} lists no computer systems"),
            }
        })?;
        Ok(systems.swap_remove(index))
    }

    /// The manager the host system [`Self::host_system`] picks names first in
    /// `Links.ManagedBy`.
    async fn host_manager(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
    ) -> Result<Manager<RedfishBmc>, EndpointExplorationError> {
        let nv_error = |context| {
            move |err| map_nv_redfish_explore_error(bmc_explorer::Error::NvRedfish { context, err })
        };
        let system = self.host_system(bmc_ip_address, bmc.clone()).await?;
        let manager_id = system
            .raw()
            .links
            .as_ref()
            .and_then(|links| links.managed_by.as_ref())
            .and_then(|managers| managers.first())
            .map(|manager| manager.id().clone())
            .ok_or_else(|| EndpointExplorationError::Other {
                details: format!("BMC {bmc_ip_address} names no manager for its host system"),
            })?;
        let missing_manager = || EndpointExplorationError::Other {
            details: format!("BMC {bmc_ip_address} does not list its host's manager {manager_id}"),
        };
        let (nv_pool, credentials) = self.nv_pool_and_credentials(BmcAccess::Established(bmc));
        nv_pool
            .service_root(bmc_ip_address, credentials)
            .await
            .map_err(|err| EndpointExplorationError::Other {
                details: format!("Cannot Redfish service root: {err}"),
            })?
            .managers()
            .await
            .map_err(nv_error("managers"))?
            .ok_or_else(missing_manager)?
            .members()
            .await
            .map_err(nv_error("managers members"))?
            .into_iter()
            .find(|manager| manager.raw().odata_id == manager_id)
            .ok_or_else(missing_manager)
    }

    pub(super) async fn disable_secure_boot(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        bmc: EstablishedBmc,
        action: libredfish::EnabledDisabled,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.lockdown(action).await.map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn lockdown_status(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
    ) -> Result<LockdownStatus, EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        bmc: EstablishedBmc,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        bmc: EstablishedBmc,
    ) -> Result<Option<bool>, EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        bmc: EstablishedBmc,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        bmc: EstablishedBmc,
        boot_interface: &BootInterfaceTarget,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        bmc: EstablishedBmc,
        mode: NicMode,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
            .await
            .map_err(map_redfish_client_creation_error)?;

        client.set_nic_mode(mode).await.map_err(map_redfish_error)?;

        Ok(())
    }

    pub(super) async fn is_viking(
        &self,
        bmc_ip_address: SocketAddr,
        bmc: EstablishedBmc,
    ) -> Result<bool, EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        bmc: EstablishedBmc,
    ) -> Result<(), EndpointExplorationError> {
        let client = self
            .create_established_redfish_client(bmc_ip_address, bmc, None)
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
        // Reachable when `[bmc_proxy]` is enabled: established-endpoint
        // operations use the proxied pool, which rejects e.g. non-443 BMC
        // ports with this variant.
        RedfishClientCreationError::Unsupported(details) => {
            EndpointExplorationError::Other { details }
        }
    }
}

/// `report` when it identifies the BMC. A failed exploration records no
/// service root, so drivers cannot be selected for it and power calls fall
/// to plain Redfish on the host system.
fn identifying(report: Option<&EndpointExplorationReport>) -> Option<&EndpointExplorationReport> {
    report.filter(|report| report.service_root.is_some())
}

/// The libredfish `Manager.Reset` type for `reset_type`; libredfish supports
/// only graceful and forced restarts.
fn libredfish_manager_reset_type(
    reset_type: ResetType,
) -> Result<libredfish::ManagerResetType, EndpointExplorationError> {
    match reset_type {
        ResetType::GracefulRestart => Ok(libredfish::ManagerResetType::GracefulRestart),
        ResetType::ForceRestart => Ok(libredfish::ManagerResetType::ForceRestart),
        other => Err(EndpointExplorationError::Other {
            details: format!("libredfish cannot request a {other:?} BMC reset"),
        }),
    }
}

/// Classifies a driver failure the way [`map_redfish_error`] classifies the
/// libredfish one.
pub(crate) fn map_platform_error(error: PlatformError) -> EndpointExplorationError {
    match &error {
        PlatformError::Unreachable => EndpointExplorationError::Unreachable {
            details: Some(error.to_string()),
        },
        PlatformError::Auth(_) => EndpointExplorationError::Unauthorized {
            details: error.to_string(),
            response_body: None,
            response_code: None,
        },
        PlatformError::Bmc { status, .. } => EndpointExplorationError::RedfishError {
            details: error.to_string(),
            response_body: None,
            response_code: Some(*status),
        },
        _ => EndpointExplorationError::RedfishError {
            details: error.to_string(),
            response_body: None,
            response_code: None,
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
                EndpointExplorationError::ConnectionTimeout {
                    details,
                    consecutive_count: 1,
                }
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

fn nv_bmc_explore_config(
    boot_interface: Option<&BootInterfaceTarget>,
    explore_drive_firmware: bool,
) -> bmc_explorer::Config<'static, carbide_redfish::nv_redfish::RedfishBmc> {
    bmc_explorer::Config {
        boot_interface_mac: boot_interface.map(BootInterfaceTarget::mac_address),
        error_classifier: &bmc_explorer::nv_redfish_error_class,
        // Chosen arbitrarily: we want to wait a bit between tries,
        // but not for too long relative to the total exploration
        // time.
        retry_timeout: Duration::from_millis(1000),
        explore_drive_firmware,
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
                        EndpointExplorationError::ConnectionTimeout {
                            details,
                            consecutive_count: 1,
                        }
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
    use std::sync::{Arc, Mutex};

    use arc_swap::ArcSwap;
    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use axum::http::header::CONTENT_LENGTH;
    use axum::http::{Method, StatusCode};
    use axum::middleware::{self, Next};
    use axum::response::{IntoResponse, Response};
    use bmc_mock::test_support::{dell_poweredge_r750_router_with_event_limits, serve_https};
    use bmc_mock::{CombinedServer, EventServiceConfig};
    use bmc_runtime::DriverPools;
    use carbide_redfish::libredfish::test_support::RedfishSim;
    use carbide_redfish::nv_redfish::NvRedfishClientPool;
    use carbide_secrets::credentials::Credentials;
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async, value_scenarios};
    use libredfish::model::service_root::RedfishVendor;
    use mac_address::MacAddress;
    use model::machine_boot_interface::{MachineBootInterface, MachineBootInterfaceTarget};
    use model::site_explorer::{
        ComputerSystem as ExploredSystem, Manager as ExploredManager, ServiceRoot,
    };
    use serde_json::{Value, json};

    use super::{
        BmcAccess, BmcCredentialType, BootInterfaceTarget, CredentialKey, DriverConnector,
        EndpointExplorationError, EndpointExplorationReport, EstablishedBmc, MachineSetupStatus,
        PowerState, ProxiedPools, RedfishClient, ResetType, nv_bmc_explore_config,
        record_evaluated_boot_interface,
    };

    fn test_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 443)
    }

    fn build_redfish_client(sim: Arc<RedfishSim>) -> RedfishClient {
        let proxy_address = Arc::new(ArcSwap::new(Arc::new(None)));
        let nv_pool = Arc::new(NvRedfishClientPool::new(proxy_address));
        RedfishClient::new(sim, nv_pool, None)
    }

    #[test]
    fn nvredfish_projects_the_mac_and_records_the_logical_target() {
        let mac_address = MacAddress::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01]);
        let boot_interface = MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.7-1-1".to_string(),
        };

        value_scenarios!(run = |target: Option<BootInterfaceTarget>| {
            let config = nv_bmc_explore_config(target.as_ref(), false);
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

    // --- established-vs-direct routing ------------------------------------

    fn nv_pool() -> Arc<NvRedfishClientPool> {
        Arc::new(NvRedfishClientPool::new(Arc::new(ArcSwap::new(Arc::new(
            None,
        )))))
    }

    fn established(mac: MacAddress, credentials: Credentials) -> EstablishedBmc {
        EstablishedBmc {
            bmc_mac_address: mac,
            credentials,
        }
    }

    /// A client with `[bmc_proxy]` enabled, built from two distinct sims so
    /// tests can observe which pool an operation routed through.
    fn proxied_client() -> (
        RedfishClient,
        Arc<RedfishSim>,
        Arc<RedfishSim>,
        Arc<NvRedfishClientPool>,
    ) {
        let ops_sim = Arc::new(RedfishSim::default());
        let general_sim = Arc::new(RedfishSim::default());
        let proxied_nv = nv_pool();
        let client = RedfishClient::new(
            ops_sim.clone(),
            nv_pool(),
            Some(ProxiedPools {
                redfish: general_sim.clone(),
                nv_redfish: proxied_nv.clone(),
            }),
        );
        (client, ops_sim, general_sim, proxied_nv)
    }

    /// With `[bmc_proxy]` enabled, established-endpoint operations
    /// authenticate by credential key on the PROXIED pool -- naming this
    /// BMC's stored root key, which the proxy resolves itself -- and never
    /// touch the direct ops pool.
    #[tokio::test]
    async fn established_operations_use_the_proxied_pool_with_key_auth() {
        use carbide_redfish::libredfish::test_support::RedfishAuthKind;
        let (client, ops_sim, general_sim, _) = proxied_client();
        let mac: MacAddress = "02:00:00:00:00:07".parse().unwrap();

        client
            .get_power_state(
                test_addr(),
                established(mac, Credentials::new("root", "pw")),
                None,
            )
            .await
            .expect("established read succeeds on the sim");

        assert!(
            ops_sim.create_client_calls().is_empty(),
            "an established read must not touch the direct ops pool"
        );
        let calls = general_sim.create_client_calls();
        assert_eq!(calls.len(), 1, "one client from the proxied pool");
        assert_eq!(calls[0].auth, RedfishAuthKind::Key);
        assert_eq!(
            calls[0].auth_key.as_deref(),
            Some(
                CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: mac,
                    },
                }
                .to_key_str()
                .as_ref()
            ),
            "the key must name this BMC's stored root credential"
        );
    }

    /// With `[bmc_proxy]` disabled, an established operation is exactly the
    /// pre-split call: the credential the caller already resolved, sent as
    /// explicit auth on the direct pool -- no key resolution, no second
    /// store read.
    #[tokio::test]
    async fn established_operations_dial_direct_with_carried_credentials_when_disabled() {
        use carbide_redfish::libredfish::test_support::RedfishAuthKind;
        let ops_sim = Arc::new(RedfishSim::default());
        let client = RedfishClient::new(ops_sim.clone(), nv_pool(), None);
        let mac: MacAddress = "02:00:00:00:00:09".parse().unwrap();

        client
            .get_power_state(
                test_addr(),
                established(mac, Credentials::new("root", "pw")),
                None,
            )
            .await
            .expect("established read succeeds on the sim");

        let calls = ops_sim.create_client_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].auth,
            RedfishAuthKind::Direct,
            "disabled mode must send the carried credential directly, as before"
        );
    }

    /// Credential-setup traffic (explicit credentials) stays on the direct
    /// ops pool even with `[bmc_proxy]` enabled.
    #[tokio::test]
    async fn direct_credential_operations_stay_on_the_ops_pool() {
        use carbide_redfish::libredfish::test_support::RedfishAuthKind;
        let (client, ops_sim, general_sim, _) = proxied_client();

        client
            .validate_bmc_credentials(test_addr(), Credentials::new("root", "password"))
            .await
            .expect("validation succeeds on the sim");

        assert!(
            general_sim.create_client_calls().is_empty(),
            "credential validation must not route via the proxied pool"
        );
        let calls = ops_sim.create_client_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].auth, RedfishAuthKind::Direct);
    }

    /// The nv-redfish selection: established + proxy -> proxied pool with no
    /// credentials; established without proxy -> direct pool with the
    /// carried credential (pre-split behavior); direct -> direct pool with
    /// the explicit credential.
    #[test]
    fn nv_pool_selection_follows_the_access_class() {
        let mac: MacAddress = "02:00:00:00:00:08".parse().unwrap();
        let stored = Credentials::new("root", "stored-password");

        let (client, _, _, proxied_nv) = proxied_client();
        let (pool, credentials) = client
            .nv_pool_and_credentials(BmcAccess::Established(established(mac, stored.clone())));
        assert!(std::ptr::eq(pool, proxied_nv.as_ref()));
        assert!(
            credentials.is_none(),
            "the proxy resolves the BMC's credentials itself"
        );

        let direct_client = RedfishClient::new(Arc::new(RedfishSim::default()), nv_pool(), None);
        let (pool, credentials) = direct_client
            .nv_pool_and_credentials(BmcAccess::Established(established(mac, stored.clone())));
        assert!(std::ptr::eq(
            pool,
            direct_client.nv_redfish_client_pool.as_ref()
        ));
        assert_eq!(
            credentials,
            Some(stored),
            "disabled mode carries the resolved credential through"
        );

        let explicit = Credentials::new("root", "factory");
        let (pool, credentials) =
            client.nv_pool_and_credentials(BmcAccess::Direct(explicit.clone()));
        assert!(std::ptr::eq(pool, client.nv_redfish_client_pool.as_ref()));
        assert_eq!(credentials, Some(explicit));
    }

    // --- drivers backend ---------------------------------------------------

    /// A drivers-backed client against a mock Dell PowerEdge R750, recording
    /// the path and body of every action the mock receives.
    struct DriversBmc {
        _server: CombinedServer,
        address: SocketAddr,
        client: RedfishClient,
        credentials: Credentials,
        actions: Arc<Mutex<Vec<(String, String)>>>,
    }

    fn drivers_bmc() -> DriversBmc {
        let (router, _state) =
            dell_poweredge_r750_router_with_event_limits(EventServiceConfig::default());
        let actions: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let recorded = actions.clone();
        // The mock answers actions with `{}`, which the production client
        // rejects; real BMCs answer with 204 or a Redfish success envelope.
        // It also omits the system's `Links.ManagedBy`, which iDRACs report.
        let router = router.layer(middleware::from_fn(move |request: Request, next: Next| {
            let recorded = recorded.clone();
            async move {
                if request.method() == Method::GET
                    && request.uri().path() == "/redfish/v1/Systems/System.Embedded.1"
                {
                    let (mut parts, body) = next.run(request).await.into_parts();
                    let body = to_bytes(body, usize::MAX)
                        .await
                        .expect("the system body is readable");
                    let mut system: Value =
                        serde_json::from_slice(&body).expect("the system body is JSON");
                    system["Links"]["ManagedBy"] =
                        json!([{"@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1"}]);
                    parts.headers.remove(CONTENT_LENGTH);
                    return Response::from_parts(parts, Body::from(system.to_string()));
                }
                if request.method() != Method::POST {
                    return next.run(request).await;
                }
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX)
                    .await
                    .expect("the action body is readable");
                recorded.lock().unwrap().push((
                    parts.uri.path().to_string(),
                    String::from_utf8_lossy(&body).into_owned(),
                ));
                let response = next.run(Request::from_parts(parts, Body::from(body))).await;
                if response.status().is_success() {
                    StatusCode::NO_CONTENT.into_response()
                } else {
                    response
                }
            }
        }));
        let (server, base_url) = serve_https("site-explorer", router);
        let address = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            base_url.port().expect("the mock BMC names its port"),
        );
        let credentials = Credentials::new("root", "password");
        let connector = DriverConnector::new(
            DriverPools {
                routed: nv_pool(),
                direct: nv_pool(),
            },
            Arc::new(TestCredentialManager::new(credentials.clone())),
            "site-explorer-test",
            bmc_drivers::built_in_rules(),
            carbide_ipmi::test_support(),
        )
        .expect("the pools are in order");
        let mut client = build_redfish_client(Arc::new(RedfishSim::default()));
        client.driver_connector = Some(Arc::new(connector));
        DriversBmc {
            _server: server,
            address,
            client,
            credentials,
            actions,
        }
    }

    /// Under the drivers backend, a power call without a report that
    /// identifies the BMC, as remediation makes after a failed exploration,
    /// reaches the host over plain Redfish, while an identifying report still
    /// selects drivers.
    #[tokio::test]
    async fn drivers_backend_powers_an_unidentified_host_over_plain_redfish() {
        let DriversBmc {
            _server,
            address,
            client,
            credentials,
            ..
        } = drivers_bmc();
        let failed =
            EndpointExplorationReport::new_with_error(EndpointExplorationError::Unreachable {
                details: None,
            });
        let mac: MacAddress = "02:00:00:00:00:0a".parse().unwrap();
        let bmc = || established(mac, credentials.clone());

        for (scenario, report) in [
            ("never explored", None),
            ("exploration failed", Some(&failed)),
        ] {
            let state = client
                .get_power_state(address, bmc(), report)
                .await
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(state, Some(PowerState::On), "{scenario}");
            client
                .power(address, bmc(), report, ResetType::ForceRestart)
                .await
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        }

        // Drivers act on the system the report names, which plain Redfish
        // would never look for, so naming a missing one shows they ran.
        let identified = EndpointExplorationReport {
            service_root: Some(ServiceRoot::default()),
            systems: vec![ExploredSystem {
                id: "Missing".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let error = client
            .get_power_state(address, bmc(), Some(&identified))
            .await
            .expect_err("the drivers look for the system the report names");
        assert!(error.to_string().contains("Missing"), "{error}");
    }

    /// Under the drivers backend, a BMC reset whose type the drivers cannot
    /// choose, because the BMC is unidentified or the operator named a type,
    /// posts `Manager.Reset` over plain Redfish carrying only the operator's
    /// type, while an identifying report with no type selects the drivers.
    #[tokio::test]
    async fn drivers_backend_resets_over_plain_redfish_unless_the_drivers_choose() {
        let bmc = drivers_bmc();
        let mac: MacAddress = "02:00:00:00:00:0b".parse().unwrap();
        let failed =
            EndpointExplorationReport::new_with_error(EndpointExplorationError::Unreachable {
                details: None,
            });
        // Drivers reset the manager the report names, which plain Redfish
        // never looks for, so naming a missing one keeps them off the wire.
        let identified = EndpointExplorationReport {
            service_root: Some(ServiceRoot::default()),
            managers: vec![ExploredManager {
                id: "Missing".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        // `Some(type)` expects one plain `Manager.Reset` carrying `type`, if
        // any; `None` expects the drivers to post none.
        for (scenario, report, reset_type, posted) in [
            (
                "never explored leaves the type to the BMC",
                None,
                None,
                Some(None),
            ),
            (
                "a failed exploration leaves the type to the BMC",
                Some(&failed),
                None,
                Some(None),
            ),
            (
                "an operator's type is sent as given, even to an identified BMC",
                Some(&identified),
                Some(ResetType::ForceRestart),
                Some(Some("ForceRestart")),
            ),
            (
                "an identified BMC with no type is reset by its drivers",
                Some(&identified),
                None,
                None,
            ),
        ] {
            bmc.actions.lock().unwrap().clear();
            let result = bmc
                .client
                .reset_bmc(
                    bmc.address,
                    established(mac, bmc.credentials.clone()),
                    report,
                    reset_type,
                )
                .await;
            let resets: Vec<Option<Value>> = bmc
                .actions
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path.ends_with("/Actions/Manager.Reset"))
                .map(|(_, body)| serde_json::from_str(body).ok())
                .collect();
            match posted {
                Some(expected) => {
                    result.unwrap_or_else(|error| panic!("{scenario}: {error}"));
                    let [body] = resets.as_slice() else {
                        panic!("{scenario}: expected one Manager.Reset, got {resets:?}");
                    };
                    let sent = body
                        .as_ref()
                        .and_then(|body| body.get("ResetType"))
                        .and_then(Value::as_str);
                    assert_eq!(sent, expected, "{scenario}");
                }
                None => assert!(resets.is_empty(), "{scenario}: {resets:?}"),
            }
        }
    }
}
