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
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use carbide_ipmi::IPMITool;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::RedfishClientPool;
use carbide_redfish::libredfish::conv::IntoLibredfish;
use carbide_redfish::nv_redfish::NvRedfishClientPool;
use carbide_secrets::credentials::{CredentialManager, Credentials};
use libredfish::model::service_root::RedfishVendor;
use mac_address::MacAddress;
use model::expected_entity::{BmcCredentialsData, ExpectedEntity};
use model::expected_switch::ExpectedSwitch;
use model::machine::MachineInterfaceSnapshot;
use model::site_explorer::{
    EndpointExplorationError, EndpointExplorationReport, LockdownStatus, NicMode,
};
use sqlx::PgPool;

use super::EndpointExplorer;
use super::config::SiteExplorerExploreMode;
use super::credentials::{CredentialClient, get_bmc_root_credential_key};
use super::metrics::SiteExplorationMetrics;
use super::redfish::RedfishClient;

const BMC_AUTH_RETRY_DURATION: Duration = Duration::from_secs(3);

/// An `EndpointExplorer` which uses redfish APIs to query the endpoint
pub struct BmcEndpointExplorer {
    redfish_client: RedfishClient,
    ipmi_tool: Arc<dyn IPMITool>,
    credential_client: CredentialClient,
    rotate_switch_nvos_credentials: Arc<AtomicBool>,
    mode: SiteExplorerExploreMode,
    /// Whether nv-redfish reports include drive firmware inventory entries.
    explore_drive_firmware: bool,
    /// Used to record per-device BMC rotation convergence at the moment the
    /// device is moved onto the site-wide BMC root (see
    /// [`Self::set_bmc_root_credentials`]). `None` only for the standalone
    /// `bmc-explorer-cli` debug tool, which runs against an in-memory credential
    /// store and no database; in that case the rotation bookkeeping is skipped.
    database_connection: Option<PgPool>,
}

impl BmcEndpointExplorer {
    pub fn new(
        redfish_client_pool: Arc<dyn RedfishClientPool>,
        nv_redfish_client_pool: Arc<NvRedfishClientPool>,
        ipmi_tool: Arc<dyn IPMITool>,
        credential_manager: Arc<dyn CredentialManager>,
        rotate_switch_nvos_credentials: Arc<AtomicBool>,
        mode: SiteExplorerExploreMode,
        database_connection: Option<PgPool>,
    ) -> Self {
        Self {
            redfish_client: RedfishClient::new(redfish_client_pool, nv_redfish_client_pool),
            ipmi_tool,
            credential_client: CredentialClient::new(credential_manager),
            rotate_switch_nvos_credentials,
            mode,
            explore_drive_firmware: false,
            database_connection,
        }
    }

    /// Sets whether nv-redfish reports include drive firmware inventory
    /// entries; off unless set.
    pub fn with_drive_firmware(mut self, explore_drive_firmware: bool) -> Self {
        self.explore_drive_firmware = explore_drive_firmware;
        self
    }

    pub async fn get_sitewide_bmc_password(&self) -> Result<String, EndpointExplorationError> {
        let version = self.current_sitewide_bmc_version().await?;
        let credentials = self
            .credential_client
            .get_sitewide_bmc_root_credentials(version)
            .await?;

        let (_, password) = match credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        Ok(password)
    }

    /// Resolve which site-wide BMC root version is currently live from
    /// `sitewide_credential_rotation.target_version`. This is the table-driven
    /// "current site-wide credential" lookup: rather than reading a fixed
    /// unversioned alias, ingestion consults the rotation table so a device
    /// ingested after a rotation lands on the version the fleet moved to (and is
    /// then recorded at that version by [`Self::set_bmc_root_credentials`]).
    ///
    /// A `target_version` of 0 means "no rotation yet" (the legacy unversioned
    /// path). The backfill migration seeds a row at version 0 for every active
    /// credential type, so a *missing* row is a broken/unmigrated database and is
    /// surfaced as an error rather than silently assuming 0 (matching the write
    /// path in [`Self::set_bmc_root_credentials`] and the rest of the rotation
    /// code, which never guess a version). The only 0 fallback is the standalone
    /// `bmc-explorer-cli` debug tool, which has no database at all.
    async fn current_sitewide_bmc_version(&self) -> Result<u32, EndpointExplorationError> {
        let Some(database_connection) = &self.database_connection else {
            return Ok(0);
        };
        let read_err = |cause: String| EndpointExplorationError::Other {
            details: format!("failed to read site-wide BMC rotation target: {cause}"),
        };
        // Single read; needs no transaction (the convergence write in
        // set_bmc_root_credentials uses one because it commits a row).
        let mut conn = database_connection
            .acquire()
            .await
            .map_err(|e| read_err(e.to_string()))?;
        let target_version = db::credential_rotation::current_target_version(
            &mut conn,
            db::credential_rotation::CredentialRotationType::Bmc,
        )
        .await
        .map_err(|e| read_err(e.to_string()))?
        .ok_or_else(|| {
            read_err(
                "no site-wide BMC rotation target row exists; the backfill migration seeds one \
                 for every active credential type, so a missing row indicates a broken or \
                 unmigrated database"
                    .to_string(),
            )
        })?;
        // The column is constrained non-negative, so a failed conversion means a
        // corrupt value, not "no rotation" -- surface it rather than masking it as
        // the legacy v0 path.
        u32::try_from(target_version).map_err(|_| {
            read_err(format!(
                "site-wide BMC rotation target version {target_version} is negative; the column \
                 is constrained non-negative, so this indicates a corrupt database"
            ))
        })
    }

    fn get_default_hardware_dpu_bmc_root_credentials(&self) -> BmcCredentialsData<'static> {
        self.credential_client
            .get_default_hardware_dpu_bmc_root_credentials()
    }

    pub async fn get_bmc_root_credentials(
        &self,
        bmc_mac_address: MacAddress,
    ) -> Result<Credentials, EndpointExplorationError> {
        self.credential_client
            .get_bmc_root_credentials(bmc_mac_address)
            .await
    }

    pub async fn get_switch_nvos_admin_credentials(
        &self,
        bmc_mac_address: MacAddress,
    ) -> Result<Credentials, EndpointExplorationError> {
        self.credential_client
            .get_switch_nvos_admin_credentials(bmc_mac_address)
            .await
    }

    pub async fn set_bmc_root_credentials(
        &self,
        bmc_mac_address: MacAddress,
        credentials: &Credentials,
    ) -> Result<(), EndpointExplorationError> {
        self.credential_client
            .set_bmc_root_credentials(bmc_mac_address, credentials)
            .await?;

        // The device is now on the site-wide BMC root (just changed on the
        // hardware, or validated as already-set on reingest) and its per-device
        // secret is in Vault. Record bmc convergence at the current site-wide
        // target version so the rotation engine tracks every host, DPU, switch,
        // and power shelf from the moment NICo owns its BMC password. Idempotent,
        // so reexploration of an already-recorded device is a no-op. Skipped only
        // by the no-database `bmc-explorer-cli` debug tool.
        if let Some(database_connection) = &self.database_connection {
            let record_err = |cause: String| EndpointExplorationError::SetCredentials {
                key: format!("device_credential_rotation/bmc/{bmc_mac_address}"),
                cause,
            };
            let mut txn = db::Transaction::begin(database_connection)
                .await
                .map_err(|e| record_err(e.to_string()))?;
            db::credential_rotation::record_device_converged(
                &mut txn,
                bmc_mac_address,
                db::credential_rotation::CredentialRotationType::Bmc,
            )
            .await
            .map_err(|e| record_err(e.to_string()))?;
            txn.commit().await.map_err(|e| record_err(e.to_string()))?;
        }

        Ok(())
    }

    pub async fn set_bmc_root_password(
        &self,
        bmc_ip_address: SocketAddr,
        vendor: RedfishVendor,
        current_bmc_credentials: Credentials,
        new_password: String,
    ) -> Result<Credentials, EndpointExplorationError> {
        self.redfish_client
            .set_bmc_root_password(
                bmc_ip_address,
                vendor,
                current_bmc_credentials.clone(),
                new_password.clone(),
            )
            .await?;

        let (user, _) = match current_bmc_credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        Ok(Credentials::UsernamePassword {
            username: user,
            password: new_password,
        })
    }

    pub async fn generate_exploration_report(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface_mac: Option<MacAddress>,
        vendor: Option<RedfishVendor>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        match self.mode {
            SiteExplorerExploreMode::LibRedfish => {
                self.redfish_client
                    .generate_exploration_report(
                        bmc_ip_address,
                        credentials.clone(),
                        boot_interface_mac,
                        vendor,
                    )
                    .await
            }
            SiteExplorerExploreMode::NvRedfish => {
                self.redfish_client
                    .nv_generate_exploration_report(
                        bmc_ip_address,
                        credentials,
                        boot_interface_mac,
                        self.explore_drive_firmware,
                    )
                    .await
            }
            SiteExplorerExploreMode::CompareResult => {
                let libredfish = self
                    .redfish_client
                    .generate_exploration_report(
                        bmc_ip_address,
                        credentials.clone(),
                        boot_interface_mac,
                        vendor,
                    )
                    .await;
                let nvredfish = self
                    .redfish_client
                    .nv_generate_exploration_report(
                        bmc_ip_address,
                        credentials,
                        boot_interface_mac,
                        self.explore_drive_firmware,
                    )
                    .await;
                match (&libredfish, &nvredfish) {
                    (Ok(report), Ok(nv_report)) => warn_report_diff(report, nv_report),
                    (Ok(_), Err(_)) => {
                        tracing::warn!(
                            "libredfish returned success when nv-redfish error: {nvredfish:?}"
                        );
                    }
                    (Err(_), Ok(_)) => {
                        tracing::warn!(
                            "libredfish returned error: {libredfish:?}, when nv-redfish success"
                        );
                    }
                    (Err(_), Err(_)) => (),
                }
                libredfish
            }
        }
    }

    // Handle machines that still have their bmc root password set to the factory default.
    // (1) For hosts, the factory default must exist in the expected machines table (expected_machine). Otherwise, return an error.
    // (2) For DPUs, try the hardware default root credentials.
    // At this point, we dont know if the machine is a host or dpu. So, try both (1) and (2).
    // If neither credentials work, return an error.
    // If we can log in using the factory credentials:
    // (1) use Redfish to set the machine's bmc root password to be the sitewide bmc root password.
    // (2) update the BMC specific root password path in vault
    async fn set_sitewide_bmc_root_password(
        &self,
        bmc_ip_address: SocketAddr,
        bmc_mac_address: MacAddress,
        vendor: RedfishVendor,
        cred_data: BmcCredentialsData<'_>,
    ) -> Result<Credentials, EndpointExplorationError> {
        if cred_data.password.is_empty() {
            return Err(EndpointExplorationError::MissingCredentials {
                key: "expected_entity_password".to_string(),
                cause: format!(
                    "Expected entity for {bmc_mac_address} has no BMC password configured"
                ),
            });
        }

        let current_bmc_credentials = Credentials::UsernamePassword {
            username: cred_data.username.to_string(),
            password: cred_data.password.to_string(),
        };
        let retain_credentials = cred_data.retain_credentials;
        tracing::info!(%bmc_ip_address, %bmc_mac_address, %vendor, "attempting to set the administrative credentials to the site password");
        let bmc_credentials = if retain_credentials {
            tracing::info!(
                %bmc_ip_address, %bmc_mac_address, %vendor,
                "bmc_retain_credentials is set; skipping BMC password rotation + storing existing credentials"
            );
            current_bmc_credentials
        } else {
            // use redfish to set the machine's BMC root password to
            // match Forge's sitewide BMC root password (from the factory default).
            // return an error if we cannot log into the machine's BMC using current credentials
            let sitewide_bmc_password = self.get_sitewide_bmc_password().await?;
            let rotated = self
                .set_bmc_root_password(
                    bmc_ip_address,
                    vendor,
                    current_bmc_credentials,
                    sitewide_bmc_password,
                )
                .await?;

            tracing::info!(
                %bmc_ip_address, %bmc_mac_address, %vendor,
                "Site explorer successfully updated the root password for {bmc_mac_address} to the sitewide BMC root password"
            );
            rotated
        };

        // set the BMC root credentials in vault for this machine
        self.set_bmc_root_credentials(bmc_mac_address, &bmc_credentials)
            .await?;

        Ok(bmc_credentials)
    }

    /// Fallback for reingested hardware: try the configured sitewide BMC root
    /// password with the expected/factory username. If the BMC is already on
    /// the sitewide password, we just need to re-populate the per-BMC vault entry.
    async fn try_sitewide_bmc_root_credentials(
        &self,
        bmc_ip_address: SocketAddr,
        bmc_mac_address: MacAddress,
        username: &str,
    ) -> Result<Credentials, EndpointExplorationError> {
        tracing::info!(
            %bmc_ip_address, %bmc_mac_address,
            "Attempting sitewide BMC root credentials fallback for possible reingested hardware"
        );

        let version = self.current_sitewide_bmc_version().await?;
        let sitewide_credentials = self
            .credential_client
            .get_sitewide_bmc_root_credentials(version)
            .await?;
        let Credentials::UsernamePassword { password, .. } = sitewide_credentials;
        let credentials = Credentials::UsernamePassword {
            username: username.to_string(),
            password,
        };

        // Some BMCs (notably HPE iLO) enforce a brief auth-failure throttle
        // after an attempt fails. Wait long enough to clear it
        // before validating with the sitewide credentials.
        tokio::time::sleep(BMC_AUTH_RETRY_DURATION).await;

        self.redfish_client
            .validate_bmc_credentials(bmc_ip_address, credentials.clone())
            .await?;

        self.set_bmc_root_credentials(bmc_mac_address, &credentials)
            .await?;

        tracing::info!(
            %bmc_ip_address, %bmc_mac_address,
            "Sitewide BMC root credentials succeeded - stored per-BMC vault entry"
        );

        Ok(credentials)
    }

    // Handle switch NVOS admin credentials setup
    // Store NVOS admin credentials in vault for the switch if they exist in expected_switch
    pub async fn set_sitewide_switch_nvos_admin_credentials(
        &self,
        bmc_mac_address: MacAddress,
        expected_switch: &ExpectedSwitch,
    ) -> Result<(), EndpointExplorationError> {
        if let (Some(nvos_username), Some(nvos_password)) = (
            expected_switch.nvos_username.as_ref(),
            expected_switch.nvos_password.as_ref(),
        ) {
            tracing::info!(
                %bmc_mac_address,
                "Storing NVOS admin credentials in vault for switch {bmc_mac_address}"
            );
            self.credential_client
                .set_bmc_nvos_admin_credentials(
                    bmc_mac_address,
                    &Credentials::UsernamePassword {
                        username: nvos_username.clone(),
                        password: nvos_password.clone(),
                    },
                )
                .await?;
        }
        Ok(())
    }

    pub async fn redfish_reset_bmc(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .reset_bmc(bmc_ip_address, credentials)
            .await
    }

    pub async fn redfish_power_control(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        action: libredfish::SystemPowerControl,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .power(bmc_ip_address, credentials, action)
            .await
    }

    pub async fn machine_setup(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .machine_setup(bmc_ip_address, credentials, boot_interface)
            .await
    }

    pub async fn set_boot_order_dpu_first(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface: &BootInterfaceTarget,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .set_boot_order_dpu_first(bmc_ip_address, credentials, boot_interface)
            .await
    }

    pub async fn set_nic_mode(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        mode: NicMode,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .set_nic_mode(bmc_ip_address, credentials, mode.into_libredfish())
            .await
    }

    async fn is_viking(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<bool, EndpointExplorationError> {
        self.redfish_client
            .is_viking(bmc_ip_address, credentials)
            .await
    }

    pub async fn clear_nvram(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .clear_nvram(bmc_ip_address, credentials)
            .await
    }

    pub async fn disable_secure_boot(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .disable_secure_boot(bmc_ip_address, credentials)
            .await
    }

    pub async fn lockdown(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        action: libredfish::EnabledDisabled,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .lockdown(bmc_ip_address, credentials, action)
            .await
    }

    pub async fn lockdown_status(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<LockdownStatus, EndpointExplorationError> {
        self.redfish_client
            .lockdown_status(bmc_ip_address, credentials)
            .await
    }

    pub async fn enable_infinite_boot(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .enable_infinite_boot(bmc_ip_address, credentials)
            .await
    }

    pub async fn is_infinite_boot_enabled(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
    ) -> Result<Option<bool>, EndpointExplorationError> {
        self.redfish_client
            .is_infinite_boot_enabled(bmc_ip_address, credentials)
            .await
    }

    async fn create_bmc_user(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        new_username: &str,
        new_password: &str,
        role_id: libredfish::RoleId,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .create_bmc_user(
                bmc_ip_address,
                credentials,
                new_username,
                new_password,
                role_id,
            )
            .await
    }

    async fn delete_bmc_user(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        delete_username: &str,
    ) -> Result<(), EndpointExplorationError> {
        self.redfish_client
            .delete_bmc_user(bmc_ip_address, credentials, delete_username)
            .await
    }
}

#[async_trait::async_trait]
impl EndpointExplorer for BmcEndpointExplorer {
    async fn check_preconditions(
        &self,
        metrics: &mut SiteExplorationMetrics,
    ) -> Result<(), EndpointExplorationError> {
        self.credential_client.check_preconditions(metrics).await
    }

    async fn have_credentials(&self, interface: &MachineInterfaceSnapshot) -> bool {
        self.get_bmc_root_credentials(interface.mac_address)
            .await
            .is_ok()
    }

    // 1) Authenticate and set the BMC root account credentials
    // 2) Authenticate and set the BMC forge-admin account credentials (TODO)
    #[tracing::instrument(skip_all, fields(object_id=%bmc_ip_address))]
    async fn explore_endpoint(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        expected: Option<&ExpectedEntity>,
        last_exploration_error: Option<&EndpointExplorationError>,
        boot_interface_mac: Option<MacAddress>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        // If the site explorer was previously unable to login to the root BMC account using
        // the expected credentials, wait for an operator to manually intervene.
        // This will avoid locking us out of BMCs.
        if last_exploration_error.is_some_and(|e| e.is_unauthorized()) {
            return Err(EndpointExplorationError::AvoidLockout);
        }

        let bmc_mac_address = interface.mac_address;
        let vendor = match self.redfish_client.get_redfish_vendor(bmc_ip_address).await {
            Ok(vendor) => vendor,
            Err(e) => {
                tracing::error!(%bmc_ip_address, "Failed to probe Redfish service root endpoint: {e}");

                // Lite-On power shelf BMCs don't expose Vendor details in the
                // service root, so we fall back to probing the Chassis endpoint.
                // Only attempt this for power shelf endpoints — machines and
                // switches should never need this workaround.
                //
                // In the future, if we want to expand this to other kinds of trays we can
                // expand the pattern matching logic below.
                let Some(ExpectedEntity::PowerShelf(eps)) = expected else {
                    return Err(e);
                };

                let (username, password) =
                    match self.get_bmc_root_credentials(bmc_mac_address).await {
                        Ok(Credentials::UsernamePassword { username, password }) => {
                            (username, password)
                        }
                        Err(_) => (eps.bmc_username.clone(), eps.bmc_password.clone()),
                    };

                // Lite-On and Delta power shelf BMCs don't expose vendor
                // details in the service root, so we fall back to checking the
                // Manufacturer field across all Chassis entries.
                let vendor = match self
                    .redfish_client
                    .probe_vendor_name_from_chassis(bmc_ip_address, username, password)
                    .await
                {
                    Ok(v) => v,
                    Err(chassis_err) => {
                        tracing::error!(%bmc_ip_address, "Failed to probe vendor from chassis: {chassis_err}");
                        return Err(e);
                    }
                };
                let vendor_lc = vendor.to_lowercase();
                if vendor_lc.contains("lite-on") {
                    RedfishVendor::LiteOnPowerShelf
                } else if vendor_lc.contains("delta") {
                    RedfishVendor::DeltaPowerShelf
                } else {
                    return Err(e);
                }
            }
        };

        tracing::info!(%bmc_ip_address, "Is a {vendor} BMC that supports Redfish");

        // Authenticate and set the BMC root account credentials

        // Case 1: Vault contains a path at "bmc/{bmc_mac_address}/root"
        // This machine has its BMC set to the carbide sitewide BMC root password.
        // Create the redfish client and generate the report.
        let report = match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                match self
                    .generate_exploration_report(
                        bmc_ip_address,
                        credentials,
                        boot_interface_mac,
                        Some(vendor),
                    )
                    .await
                {
                    Ok(report) => report,
                    // BMCs (HPEs currently) can return intermittent 401 errors even with valid credentials.
                    // Allow up to MAX_AUTH_RETRIES before escalating to regular Unauthorized.
                    Err(EndpointExplorationError::Unauthorized {
                        details,
                        response_body,
                        response_code,
                    }) if vendor == RedfishVendor::Hpe => {
                        const MAX_AUTH_RETRIES: u32 = 5;

                        let previous_count = last_exploration_error
                            .and_then(|e| e.intermittent_unauthorized_count())
                            .unwrap_or(0);
                        let consecutive_count = previous_count + 1;

                        if consecutive_count > MAX_AUTH_RETRIES {
                            tracing::warn!(
                                %bmc_ip_address, %bmc_mac_address, %details, consecutive_count,
                                "BMC unauthorized error persisted - escalating to Unauthorized"
                            );
                            return Err(EndpointExplorationError::Unauthorized {
                                details,
                                response_body,
                                response_code,
                            });
                        }

                        tracing::warn!(
                            %bmc_ip_address, %bmc_mac_address, %details, consecutive_count,
                            "BMC unauthorized error - treating as intermittent"
                        );
                        return Err(EndpointExplorationError::IntermittentUnauthorized {
                            details,
                            response_body,
                            response_code,
                            consecutive_count,
                        });
                    }
                    Err(e) => return Err(e),
                }
            }

            Err(EndpointExplorationError::MissingCredentials { .. }) => {
                // No per-BMC vault entry exists. Now try to:
                //   1) Login with expected/factory credentials
                //   2) Rotate the BMC root password to the sitewide root password
                //   3) Store the per-BMC vault entry
                //   4) Generate the report
                //
                // If the expected/factory credentials fail (Unauthorized), fall
                // back to the configured sitewide root password without rotation.
                // This covers reingested hardware whose per-BMC vault entry was
                // lost but whose BMC is already set to the sitewide password.

                tracing::info!(
                    %bmc_ip_address,
                    "Site explorer could not find an entry in vault at 'bmc/{bmc_mac_address}/root' - this is expected if the BMC has never been seen before.",
                );

                let bmc_cred_data = match expected {
                    Some(v) => {
                        tracing::info!(%bmc_ip_address, %bmc_mac_address, "Found an expected {} for this BMC mac address", v.name());
                        v.bmc_credentials_data()
                    }
                    None => {
                        tracing::info!(%bmc_ip_address, %bmc_mac_address, %vendor, "No expected machine found, could be a BlueField");
                        // We dont know if this machine is a DPU at this point
                        // Check the vendor to see if it could be a DPU (the DPU's vendor is NVIDIA)
                        match vendor {
                            RedfishVendor::NvidiaDpu => {
                                // This machine is a DPU.
                                // Try the DPU hardware default password to handle the DPU case
                                // This password will not work for a Viking host and we will return an error
                                self.get_default_hardware_dpu_bmc_root_credentials()
                            }
                            _ => {
                                return Err(EndpointExplorationError::MissingCredentials {
                                    key: "expected_machine".to_owned(),
                                    cause: format!(
                                        "The expected machine credentials do not exist for {vendor} machine {bmc_ip_address}/{bmc_mac_address} "
                                    ),
                                });
                            }
                        }
                    }
                };

                match self
                    .set_sitewide_bmc_root_password(
                        bmc_ip_address,
                        bmc_mac_address,
                        vendor,
                        bmc_cred_data,
                    )
                    .await
                {
                    Ok(bmc_credentials) => {
                        self.generate_exploration_report(
                            bmc_ip_address,
                            bmc_credentials,
                            None,
                            Some(vendor),
                        )
                        .await?
                    }
                    Err(
                        EndpointExplorationError::Unauthorized { .. }
                        | EndpointExplorationError::MissingCredentials { .. },
                    ) => {
                        let bmc_credentials = self
                            .try_sitewide_bmc_root_credentials(
                                bmc_ip_address,
                                bmc_mac_address,
                                bmc_cred_data.username,
                            )
                            .await?;
                        self.generate_exploration_report(
                            bmc_ip_address,
                            bmc_credentials,
                            None,
                            Some(vendor),
                        )
                        .await?
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(e) => {
                return Err(e);
            }
        };

        // Check for switch NVOS admin credentials if this is a switch
        if let Some(ExpectedEntity::Switch(expected_switch)) = expected
            && expected_switch.nvos_username.is_some()
            && expected_switch.nvos_password.is_some()
        {
            // Only check if rotation is enabled
            if self.rotate_switch_nvos_credentials.load(Ordering::Relaxed) {
                match self
                    .get_switch_nvos_admin_credentials(bmc_mac_address)
                    .await
                {
                    Ok(_) => {
                        tracing::trace!(
                            %bmc_ip_address, %bmc_mac_address,
                            "NVOS admin credentials already exist in vault for switch {bmc_mac_address}"
                        );
                    }
                    Err(_) => {
                        tracing::info!(
                            %bmc_ip_address, %bmc_mac_address,
                            "Site explorer could not find NVOS admin credentials in vault for switch {bmc_mac_address} - setting them up.",
                        );
                        self.set_sitewide_switch_nvos_admin_credentials(
                            bmc_mac_address,
                            expected_switch,
                        )
                        .await?;
                    }
                }
            }
        }

        Ok(report)
    }

    async fn redfish_reset_bmc(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.redfish_reset_bmc(bmc_ip_address, credentials).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "Site explorer does not support resetting the BMCs that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn ipmitool_reset_bmc(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;
        let credential_key = get_bmc_root_credential_key(bmc_mac_address);
        self.ipmi_tool
            .bmc_cold_reset(bmc_ip_address.ip(), &credential_key)
            .await
            .map_err(|err| EndpointExplorationError::Other {
                details: format!("ipmi_tool failed against {bmc_ip_address} failed: {err}"),
            })
    }

    async fn redfish_get_power_state(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<libredfish::PowerState, EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.redfish_client
                    .get_power_state(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "Site explorer cannot fetch live power state for an endpoint without credentials: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn redfish_power_control(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        action: libredfish::SystemPowerControl,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.redfish_power_control(bmc_ip_address, credentials, action)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "Site explorer does not support rebooting the endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn disable_secure_boot(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.disable_secure_boot(bmc_ip_address, credentials).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support disabling secure boot for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn lockdown(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        action: libredfish::EnabledDisabled,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.lockdown(bmc_ip_address, credentials, action).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support lockdown for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn lockdown_status(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<LockdownStatus, EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.lockdown_status(bmc_ip_address, credentials).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support lockdown status for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn enable_infinite_boot(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.enable_infinite_boot(bmc_ip_address, credentials).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support enabling infinite boot for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn is_infinite_boot_enabled(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<Option<bool>, EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.is_infinite_boot_enabled(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support checking infinite boot status for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn machine_setup(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.machine_setup(bmc_ip_address, credentials, boot_interface)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support starting machine_setup for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn set_boot_order_dpu_first(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        boot_interface: &BootInterfaceTarget,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.set_boot_order_dpu_first(bmc_ip_address, credentials, boot_interface)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support configuring the boot order on host BMCs that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn set_nic_mode(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        mode: NicMode,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.set_nic_mode(bmc_ip_address, credentials, mode).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support set_nic_mode for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn is_viking(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<bool, EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.is_viking(bmc_ip_address, credentials).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support is_viking for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn clear_nvram(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => self.clear_nvram(bmc_ip_address, credentials).await,
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support clear_nvram for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn create_bmc_user(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        username: &str,
        password: &str,
        role_id: libredfish::RoleId,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.create_bmc_user(bmc_ip_address, credentials, username, password, role_id)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support create_bmc_user for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }

    async fn delete_bmc_user(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        username: &str,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.delete_bmc_user(bmc_ip_address, credentials, username)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    "BMC endpoint explorer does not support delete_bmc_user for endpoints that have not been authenticated: could not find an entry in vault at 'bmc/{}/root'.",
                    bmc_mac_address,
                );
                Err(e)
            }
        }
    }
}

// This report is temporary. For transition period when we check that
// nv-redfish produces the same reports as libredfish.
fn warn_report_diff(report1: &EndpointExplorationReport, report2: &EndpointExplorationReport) {
    if report1.endpoint_type != report2.endpoint_type {
        tracing::warn!(
            "endpoint_type are not equal: {:?} != {:?}",
            report1.endpoint_type,
            report2.endpoint_type
        );
    }

    if report1.vendor != report2.vendor {
        tracing::warn!(
            "vendors are not equal: {:?} != {:?}",
            report1.vendor,
            report2.vendor
        );
    }

    if report1.managers != report2.managers {
        tracing::warn!(
            "managers are not equal: {:?} != {:?}",
            report1.managers,
            report2.managers
        );
    }

    if report1.systems.len() != report2.systems.len() {
        tracing::warn!(
            "reported different number of systems: {:?} != {:?}",
            report1.systems.len(),
            report2.systems.len(),
        );
    }

    for (s1, s2) in report1.systems.iter().zip(report2.systems.iter()) {
        if s1.id != s2.id {
            tracing::warn!("systems.id are not equal: {:?} != {:?}", s1.id, s2.id);
        } else {
            if s1.ethernet_interfaces != s2.ethernet_interfaces {
                tracing::warn!(
                    "systems[{:?}].ethernet_interfaces are not equal: {:?} != {:?}",
                    s1.id,
                    s1.ethernet_interfaces,
                    s2.ethernet_interfaces
                );
            }

            if s1.manufacturer != s2.manufacturer {
                tracing::warn!(
                    "systems[{:?}].manufacturer are not equal: {:?} != {:?}",
                    s1.id,
                    s1.manufacturer,
                    s2.manufacturer
                );
            }

            if s1.model != s2.model {
                tracing::warn!(
                    "systems[{:?}].model are not equal: {:?} != {:?}",
                    s1.id,
                    s1.model,
                    s2.model
                );
            }

            if s1.serial_number != s2.serial_number {
                tracing::warn!(
                    "systems[{:?}].serial_number are not equal: {:?} != {:?}",
                    s1.id,
                    s1.serial_number,
                    s2.serial_number
                );
            }

            if s1.attributes != s2.attributes {
                tracing::warn!(
                    "systems[{:?}].attributes are not equal: {:?} != {:?}",
                    s1.id,
                    s1.attributes,
                    s2.attributes
                );
            }

            if s1.pcie_devices != s2.pcie_devices {
                if s1.pcie_devices.len() != s2.pcie_devices.len() {
                    tracing::warn!(
                        "systems[{:?}].pcie_devices.len() are not equal: ids1: {:?}, ids2: {:?}",
                        s1.id,
                        s1.pcie_devices
                            .iter()
                            .map(|v| v.id.as_ref())
                            .collect::<Vec<_>>(),
                        s2.pcie_devices
                            .iter()
                            .map(|v| v.id.as_ref())
                            .collect::<Vec<_>>(),
                    );
                } else {
                    let s2devices = s2
                        .pcie_devices
                        .iter()
                        .map(|v| (&v.id, v))
                        .collect::<HashMap<_, _>>();
                    for s1dev in &s1.pcie_devices {
                        if let Some(s2dev) = s2devices.get(&s1dev.id) {
                            if s1dev != *s2dev {
                                tracing::warn!(
                                    "systems[{:?}].pcie_devices[{:?}] devices not equal: {:?} != {:?}",
                                    s1.id,
                                    s1dev.id,
                                    s1dev,
                                    s2dev,
                                );
                            }
                        } else {
                            tracing::warn!(
                                "systems[{:?}].pcie_devices.len() device {:?} is not found in second report",
                                s1.id,
                                s1dev.id
                            );
                        }
                    }
                }
            }

            if s1.base_mac != s2.base_mac {
                tracing::warn!(
                    "systems[{:?}].base_mac are not equal: {:?} != {:?}",
                    s1.id,
                    s1.base_mac,
                    s2.base_mac
                );
            }

            if s1.power_state != s2.power_state {
                tracing::warn!(
                    "systems[{:?}].power_state are not equal: {:?} != {:?}",
                    s1.id,
                    s1.power_state,
                    s2.power_state
                );
            }

            if s1.sku != s2.sku {
                tracing::warn!(
                    "systems[{:?}].sku are not equal: {:?} != {:?}",
                    s1.id,
                    s1.sku,
                    s2.sku
                );
            }

            if s1.boot_order != s2.boot_order {
                tracing::warn!(
                    "systems[{:?}].boot_order are not equal: {:?} != {:?}",
                    s1.id,
                    s1.boot_order,
                    s2.boot_order
                );
            }
        }
    }

    if report1.chassis.len() != report2.chassis.len() {
        tracing::warn!(
            "reported different number of chassis: {:?} != {:?}",
            report1.chassis.len(),
            report2.chassis.len(),
        );
    }

    for (c1, c2) in report1.chassis.iter().zip(report2.chassis.iter()) {
        if c1.id != c2.id {
            tracing::warn!("chassis.id are not equal: {:?} != {:?}", c1.id, c2.id);
        } else if c1 != c2 {
            tracing::warn!("chassis[{:?}] are not equal: {:?} != {:?}", c1.id, c1, c2);
        }
    }

    if report1.service.len() != report2.service.len() {
        tracing::warn!(
            "reported different number of service: {:?} != {:?}",
            report1.service.len(),
            report2.service.len(),
        );
    }

    for (s1, s2) in report1.service.iter().zip(report2.service.iter()) {
        if s1.id != s2.id {
            tracing::warn!("service.id are not equal: {:?} != {:?}", s1.id, s2.id);
        } else {
            if s1.inventories.len() != s2.inventories.len() {
                tracing::warn!("service[{:?}] are not equal: {:?} != {:?}", s1.id, s1, s2);
            }
            // Stable ordering of FW by id. Dell PowerEdge R770 doesn't
            // provide stable order of FW versions.
            let mut report1_idx = (0..s1.inventories.len()).collect::<Vec<_>>();
            report1_idx.sort_by_key(|i| &s1.inventories[*i].id);
            let mut report2_idx = (0..s2.inventories.len()).collect::<Vec<_>>();
            report2_idx.sort_by_key(|i| &s2.inventories[*i].id);

            for (i1, i2) in report1_idx.into_iter().zip(report2_idx) {
                let i1 = &s1.inventories[i1];
                let i2 = &s2.inventories[i2];
                if i1.id != i2.id
                    || i1.description != i2.description
                    || i1.version != i2.version
                    || i1
                        .release_date
                        .as_ref()
                        .and_then(|v| if v == "00:00:00Z" { None } else { Some(v) })
                        != i2
                            .release_date
                            .as_ref()
                            .and_then(|v| if v == "00:00:00Z" { None } else { Some(v) })
                {
                    tracing::warn!(
                        "service[{:?}].inventories are not equal: {:?} != {:?}",
                        s1.id,
                        i1,
                        i2
                    );
                }
            }
        }
    }

    if report1.machine_setup_status.is_some() != report2.machine_setup_status.is_some() {
        tracing::warn!(
            "forge_setup_status(es) are not equal: {:?} != {:?}",
            report1.machine_setup_status,
            report2.machine_setup_status,
        );
    } else if let Some(r1) = &report1.machine_setup_status
        && let Some(r2) = &report2.machine_setup_status
    {
        if r1.is_done != r2.is_done {
            tracing::warn!("forge_setup_status(es) are not equal: {r1:?} != {r2:?}",);
        }

        let mut sst1_idx = (0..r1.diffs.len()).collect::<Vec<_>>();
        sst1_idx.sort_by_key(|i| &r1.diffs[*i].key);
        let mut sst2_idx = (0..r2.diffs.len()).collect::<Vec<_>>();
        sst2_idx.sort_by_key(|i| &r2.diffs[*i].key);
        if sst1_idx.len() != sst2_idx.len() {
            tracing::warn!(
                "machine_setup_status diffs are not equal: {:?} != {:?}",
                r1.diffs,
                r2.diffs
            );
        } else {
            for (i1, i2) in sst1_idx.into_iter().zip(sst2_idx) {
                let d1 = &r1.diffs[i1];
                let d2 = &r2.diffs[i2];
                if d1 != d2 {
                    tracing::warn!("machine_setup_status diffs are not equal: {d1:?} != {d2:?}");
                }
            }
        }
    }

    if report1.secure_boot_status != report2.secure_boot_status {
        tracing::warn!(
            "secure_boot_status(es) are not equal: {:?} != {:?}",
            report1.secure_boot_status,
            report2.secure_boot_status,
        );
    }

    if report1.lockdown_status != report2.lockdown_status {
        tracing::warn!(
            "lockdown_status(es) are not equal: {:?} != {:?}",
            report1.lockdown_status,
            report2.lockdown_status,
        );
    }

    if report1.power_shelf_id != report2.power_shelf_id {
        tracing::warn!(
            "power_shelf_id are not equal: {:?} != {:?}",
            report1.power_shelf_id,
            report2.power_shelf_id
        )
    }

    if report1.switch_id != report2.switch_id {
        tracing::warn!(
            "switch_id are not equal: {:?} != {:?}",
            report1.switch_id,
            report2.switch_id
        )
    }

    if report1.physical_slot_number != report2.physical_slot_number {
        tracing::warn!(
            "physical_slot_number are not equal: {:?} != {:?}",
            report1.physical_slot_number,
            report2.physical_slot_number
        )
    }

    if report1.compute_tray_index != report2.compute_tray_index {
        tracing::warn!(
            "compute_tray_index are not equal: {:?} != {:?}",
            report1.compute_tray_index,
            report2.compute_tray_index
        )
    }

    if report1.topology_id != report2.topology_id {
        tracing::warn!(
            "topology_id are not equal: {:?} != {:?}",
            report1.topology_id,
            report2.topology_id
        )
    }

    if report1.revision_id != report2.revision_id {
        tracing::warn!(
            "revision_id are not equal: {:?} != {:?}",
            report1.revision_id,
            report2.revision_id
        )
    }
}
