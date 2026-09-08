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

use bmc_explorer::Product;
use carbide_ipmi::IPMITool;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::BmcCredentialOps;
use carbide_redfish::libredfish::conv::IntoLibredfish;
use carbide_redfish::nv_redfish::NvRedfishClientPool;
use carbide_secrets::credentials::{CredentialManager, Credentials};
use libredfish::model::service_root::RedfishVendor;
use mac_address::MacAddress;
use model::expected_entity::{BmcCredentialsData, ExpectedEntity};
use model::expected_switch::ExpectedSwitch;
use model::machine::MachineInterfaceSnapshot;
use model::site_explorer::{
    BlueFieldOperatingMode, EndpointExplorationError, EndpointExplorationReport, LockdownStatus,
};
use sqlx::PgPool;

use super::config::SiteExplorerExploreMode;
use super::credentials::{CredentialClient, get_bmc_root_credential_key};
use super::metrics::SiteExplorationMetrics;
use super::redfish::RedfishClient;
use super::{AuthenticatedBmc, EndpointExplorer};

const BMC_AUTH_RETRY_DURATION: Duration = Duration::from_secs(3);

/// The site explorer moved a device's BMC root password onto the site-wide
/// credential during ingestion (or failed to). Rotations are infrequent and
/// security-relevant: the counter is the audit signal by outcome, and the log
/// line carries the device address plus the error when one occurred.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "bmc_password_rotation_finished",
    metric_name = "carbide_site_explorer_bmc_password_rotations_total",
    component = "site-explorer",
    log = info,
    metric = counter,
    message = "BMC root password rotation finished",
    describe = "Number of BMC root password rotations onto the site-wide credential, by \
                outcome"
)]
struct BmcPasswordRotationFinished {
    #[label]
    outcome: carbide_instrument::Outcome,
    #[context]
    bmc_ip_address: SocketAddr,
    /// The device's stable identity (it keys the vault credential entry).
    #[context]
    bmc_mac_address: MacAddress,
    #[context]
    vendor: RedfishVendor,
    /// The rotation failure, when there was one; empty on success.
    #[context]
    error: String,
}

/// Credential resolution plus authenticated Redfish/IPMI access. Owns the
/// [`AuthenticatedBmc`] implementation, so BMC admin ops live here rather than
/// on the explorer.
#[derive(Clone)]
pub struct AuthenticatedBmcClient {
    redfish_client: RedfishClient,
    ipmi_tool: Arc<dyn IPMITool>,
    credential_client: CredentialClient,
}

impl AuthenticatedBmcClient {
    /// Build the shared authenticated BMC client used by endpoint exploration
    /// and by callers performing BMC administration.
    pub fn new(
        redfish_client_pool: Arc<dyn BmcCredentialOps>,
        nv_redfish_client_pool: Arc<NvRedfishClientPool>,
        ipmi_tool: Arc<dyn IPMITool>,
        credential_manager: Arc<dyn CredentialManager>,
    ) -> Self {
        Self {
            redfish_client: RedfishClient::new(redfish_client_pool, nv_redfish_client_pool),
            ipmi_tool,
            credential_client: CredentialClient::new(credential_manager),
        }
    }
}

/// An `EndpointExplorer` which uses redfish APIs to query the endpoint
pub struct BmcEndpointExplorer {
    bmc_client: Arc<AuthenticatedBmcClient>,
    rotate_switch_nvos_credentials: Arc<AtomicBool>,
    mode: SiteExplorerExploreMode,
    /// Used to record per-device BMC rotation convergence at the moment the
    /// device is moved onto the site-wide BMC root (see
    /// [`Self::set_bmc_root_credentials`]). `None` only for the standalone
    /// `bmc-explorer-cli` debug tool, which runs against an in-memory credential
    /// store and no database; in that case the rotation bookkeeping is skipped.
    database_connection: Option<PgPool>,
}

impl BmcEndpointExplorer {
    /// Build an explorer over the shared authenticated BMC client.
    pub fn new(
        bmc_client: Arc<AuthenticatedBmcClient>,
        rotate_switch_nvos_credentials: Arc<AtomicBool>,
        mode: SiteExplorerExploreMode,
        database_connection: Option<PgPool>,
    ) -> Self {
        Self {
            bmc_client,
            rotate_switch_nvos_credentials,
            mode,
            database_connection,
        }
    }

    pub async fn get_sitewide_bmc_password(&self) -> Result<String, EndpointExplorationError> {
        let version = self.current_sitewide_bmc_version().await?;
        let credentials = self
            .bmc_client
            .credential_client
            .get_sitewide_bmc_root_credentials(version)
            .await?;

        let (_, password) = match credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        Ok(password)
    }

    async fn get_sitewide_dpu_bmc_service_password(
        &self,
        create_if_missing: bool,
    ) -> Result<String, EndpointExplorationError> {
        let version = self.current_sitewide_dpu_bmc_service_version().await?;
        self.bmc_client
            .credential_client
            .get_sitewide_dpu_bmc_service_password(version, create_if_missing)
            .await
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

    /// Resolve which site-wide DPU BMC `service` version is currently live from
    /// `sitewide_credential_rotation.target_version` for the `dpu_bmc_service`
    /// family, mirroring [`Self::current_sitewide_bmc_version`]. A newly ingested
    /// BF4 DPU lands on this version and is recorded there by
    /// [`Self::rotate_dpu_service_password_from_factory_defaults`]. The seed
    /// migration creates a row at version 0, so a missing row is a broken or
    /// unmigrated database and is surfaced rather than assumed 0; the only 0
    /// fallback is the no-database `bmc-explorer-cli` debug tool.
    async fn current_sitewide_dpu_bmc_service_version(
        &self,
    ) -> Result<u32, EndpointExplorationError> {
        let Some(database_connection) = &self.database_connection else {
            return Ok(0);
        };
        let read_err = |cause: String| EndpointExplorationError::Other {
            details: format!("failed to read site-wide DPU BMC service rotation target: {cause}"),
        };
        let mut conn = database_connection
            .acquire()
            .await
            .map_err(|e| read_err(e.to_string()))?;
        let target_version = db::credential_rotation::current_target_version(
            &mut conn,
            db::credential_rotation::CredentialRotationType::DpuBmcService,
        )
        .await
        .map_err(|e| read_err(e.to_string()))?
        .ok_or_else(|| {
            read_err(
                "no site-wide DPU BMC service rotation target row exists; the seed migration \
                 creates one, so a missing row indicates a broken or unmigrated database"
                    .to_string(),
            )
        })?;
        u32::try_from(target_version).map_err(|_| {
            read_err(format!(
                "site-wide DPU BMC service rotation target version {target_version} is negative; \
                 the column is constrained non-negative, so this indicates a corrupt database"
            ))
        })
    }

    async fn get_dpu_factory_default_credentials(&self, bmc_ip_address: SocketAddr) -> Credentials {
        let model = self
            .bmc_client
            .redfish_client
            .get_dpu_model_hint(bmc_ip_address)
            .await;
        self.bmc_client
            .credential_client
            .get_dpu_factory_default_credentials(model)
            .await
    }

    pub async fn get_switch_nvos_admin_credentials(
        &self,
        bmc_mac_address: MacAddress,
    ) -> Result<Credentials, EndpointExplorationError> {
        self.bmc_client
            .credential_client
            .get_switch_nvos_admin_credentials(bmc_mac_address)
            .await
    }

    pub async fn set_bmc_root_credentials(
        &self,
        bmc_mac_address: MacAddress,
        credentials: &Credentials,
    ) -> Result<(), EndpointExplorationError> {
        self.bmc_client
            .credential_client
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

    async fn rotate_dpu_service_password_from_factory_defaults(
        &self,
        bmc_ip_address: SocketAddr,
        bmc_mac_address: MacAddress,
        root_credentials: &Credentials,
    ) -> Result<(), EndpointExplorationError> {
        let new_password = self.get_sitewide_dpu_bmc_service_password(true).await?;
        self.bmc_client
            .redfish_client
            .set_bf4_dpu_service_password(bmc_ip_address, root_credentials.clone(), new_password)
            .await?;

        // The BF4 DPU now carries the site-wide `service` password at the current
        // target version (the version `get_sitewide_dpu_bmc_service_password` just
        // read and applied). Record convergence for the `dpu_bmc_service` family
        // so the rotation engine tracks this DPU from the moment NICo owns the
        // account -- the "ever-after" enrollment for new BF4 DPUs (already-ingested
        // ones are enrolled by the seed migration's backfill). Idempotent, so
        // reexploration is a no-op. Skipped only by the no-database
        // `bmc-explorer-cli` debug tool.
        if let Some(database_connection) = &self.database_connection {
            let record_err = |cause: String| EndpointExplorationError::SetCredentials {
                key: format!("device_credential_rotation/dpu_bmc_service/{bmc_mac_address}"),
                cause,
            };
            let mut txn = db::Transaction::begin(database_connection)
                .await
                .map_err(|e| record_err(e.to_string()))?;
            db::credential_rotation::record_device_converged(
                &mut txn,
                bmc_mac_address,
                db::credential_rotation::CredentialRotationType::DpuBmcService,
            )
            .await
            .map_err(|e| record_err(e.to_string()))?;
            txn.commit().await.map_err(|e| record_err(e.to_string()))?;
        }

        Ok(())
    }

    pub async fn generate_exploration_report(
        &self,
        bmc_ip_address: SocketAddr,
        credentials: Credentials,
        boot_interface: Option<&BootInterfaceTarget>,
        vendor: Option<RedfishVendor>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        match self.mode {
            SiteExplorerExploreMode::LibRedfish => {
                self.bmc_client
                    .redfish_client
                    .generate_exploration_report(
                        bmc_ip_address,
                        credentials.clone(),
                        boot_interface,
                        vendor,
                    )
                    .await
            }
            SiteExplorerExploreMode::NvRedfish => {
                self.bmc_client
                    .redfish_client
                    .nv_generate_exploration_report(bmc_ip_address, credentials, boot_interface)
                    .await
            }
            SiteExplorerExploreMode::CompareResult => {
                let libredfish = self
                    .bmc_client
                    .redfish_client
                    .generate_exploration_report(
                        bmc_ip_address,
                        credentials.clone(),
                        boot_interface,
                        vendor,
                    )
                    .await;
                let nvredfish = self
                    .bmc_client
                    .redfish_client
                    .nv_generate_exploration_report(bmc_ip_address, credentials, boot_interface)
                    .await;
                match (&libredfish, &nvredfish) {
                    (Ok(report), Ok(nv_report)) => warn_report_diff(report, nv_report),
                    (Ok(_), Err(_)) => {
                        tracing::warn!(
                            nvredfish = ?nvredfish,
                            "libredfish succeeded while nv-redfish returned an error"
                        );
                    }
                    (Err(_), Ok(_)) => {
                        tracing::warn!(
                            libredfish = ?libredfish,
                            "libredfish returned an error while nv-redfish succeeded"
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
            let rotation = self
                .bmc_client
                .set_bmc_root_password(
                    bmc_ip_address,
                    vendor,
                    current_bmc_credentials,
                    sitewide_bmc_password,
                )
                .await;
            carbide_instrument::emit(BmcPasswordRotationFinished {
                outcome: carbide_instrument::Outcome::from(&rotation),
                bmc_ip_address,
                bmc_mac_address,
                vendor,
                error: rotation
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            });
            rotation?
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
            .bmc_client
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

        self.bmc_client
            .redfish_client
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
                "Storing NVOS admin credentials in vault"
            );
            self.bmc_client
                .credential_client
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
}

impl AuthenticatedBmcClient {
    async fn get_bmc_root_credentials(
        &self,
        bmc_mac_address: MacAddress,
    ) -> Result<Credentials, EndpointExplorationError> {
        self.credential_client
            .get_bmc_root_credentials(bmc_mac_address)
            .await
    }

    async fn set_bmc_root_password(
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
}

#[async_trait::async_trait]
impl EndpointExplorer for BmcEndpointExplorer {
    async fn check_preconditions(
        &self,
        metrics: &mut SiteExplorationMetrics,
    ) -> Result<(), EndpointExplorationError> {
        self.bmc_client
            .credential_client
            .check_preconditions(metrics)
            .await
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
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError> {
        // If the site explorer was previously unable to login to the root BMC account using
        // the expected credentials, wait for an operator to manually intervene.
        // This will avoid locking us out of BMCs.
        if last_exploration_error.is_some_and(|e| e.is_unauthorized()) {
            return Err(EndpointExplorationError::AvoidLockout);
        }

        let bmc_mac_address = interface.mac_address;
        let vendor = match self
            .bmc_client
            .redfish_client
            .get_redfish_vendor(bmc_ip_address)
            .await
        {
            Ok(vendor) => vendor,
            Err(e) => {
                tracing::error!(
                    %bmc_ip_address,
                    error = %e,
                    "Failed to probe Redfish service root endpoint"
                );

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

                let (username, password) = match self
                    .bmc_client
                    .get_bmc_root_credentials(bmc_mac_address)
                    .await
                {
                    Ok(Credentials::UsernamePassword { username, password }) => {
                        (username, password)
                    }
                    Err(_) => (eps.bmc_username.clone(), eps.bmc_password.clone()),
                };

                // Lite-On and Delta power shelf BMCs don't expose vendor
                // details in the service root, so we fall back to checking the
                // Manufacturer field across all Chassis entries.
                let vendor = match self
                    .bmc_client
                    .redfish_client
                    .probe_vendor_name_from_chassis(bmc_ip_address, username, password)
                    .await
                {
                    Ok(v) => v,
                    Err(chassis_err) => {
                        tracing::error!(
                            %bmc_ip_address,
                            error = %chassis_err,
                            "Failed to probe vendor from chassis"
                        );
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

        tracing::debug!(
            target: "carbide_diagnostics::bmc_redfish_supported",
            %bmc_ip_address,
            %vendor,
            "BMC supports Redfish"
        );

        // Authenticate and set the BMC root account credentials

        // Case 1: Vault contains a path at "bmc/{bmc_mac_address}/root"
        // This machine has its BMC set to the carbide sitewide BMC root password.
        // Create the redfish client and generate the report.
        let report = match self
            .bmc_client
            .get_bmc_root_credentials(bmc_mac_address)
            .await
        {
            Ok(credentials) => {
                match self
                    .generate_exploration_report(
                        bmc_ip_address,
                        credentials,
                        boot_interface,
                        Some(vendor),
                    )
                    .await
                {
                    Ok(report) => report,
                    // BMCs (HPE iLO, NVIDIA GB200/GB300, Vera Rubin, Lenovo AMI,
                    // Viking AMI) can return intermittent 401 errors even with
                    // valid credentials.
                    // Allow up to MAX_AUTH_RETRIES before escalating to regular Unauthorized.
                    Err(EndpointExplorationError::Unauthorized {
                        details,
                        response_body,
                        response_code,
                    }) if matches!(
                        vendor,
                        RedfishVendor::Hpe
                            | RedfishVendor::NvidiaGBx00
                            | RedfishVendor::LenovoGB300
                            | RedfishVendor::LenovoAMI
                            | RedfishVendor::AMI
                            | RedfishVendor::VeraRubin
                    ) =>
                    {
                        const MAX_AUTH_RETRIES: u32 = 5;

                        let previous_count = last_exploration_error
                            .and_then(|e| e.intermittent_unauthorized_count())
                            .unwrap_or(0);
                        let consecutive_count = previous_count + 1;

                        if consecutive_count > MAX_AUTH_RETRIES {
                            tracing::warn!(
                                %bmc_ip_address,
                                %bmc_mac_address,
                                reason = %details,
                                consecutive_unauthorized_count = consecutive_count,
                                "BMC unauthorized error persisted - escalating to Unauthorized"
                            );
                            return Err(EndpointExplorationError::Unauthorized {
                                details,
                                response_body,
                                response_code,
                            });
                        }

                        tracing::warn!(
                            %bmc_ip_address,
                            %bmc_mac_address,
                            reason = %details,
                            consecutive_unauthorized_count = consecutive_count,
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
                //   4) On BF4, rotate the BMC `service` account (required for
                //      SSH access) to the site-wide DPU BMC service password
                //   5) Generate the report
                //
                // If the expected/factory credentials fail (Unauthorized), fall
                // back to the configured sitewide root password without rotation.
                // This covers reingested hardware whose per-BMC vault entry was
                // lost but whose BMC is already set to the sitewide password.

                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    "Site explorer could not find a BMC root credential entry in vault - this is expected if the BMC has never been seen before.",
                );

                // When no expected entity is present and the vendor is a DPU, look up the
                // per-model factory default from vault (or fall back to the hardcoded default).
                // Declared before `bmc_cred_data` so it outlives the borrow.
                let dpu_factory_creds = if expected.is_none() && vendor == RedfishVendor::NvidiaDpu
                {
                    Some(
                        self.get_dpu_factory_default_credentials(bmc_ip_address)
                            .await,
                    )
                } else {
                    None
                };

                let product = self
                    .bmc_client
                    .redfish_client
                    .get_redfish_product(bmc_ip_address)
                    .await?;
                let is_bf4_dpu = bmc_explorer::is_bf4_product(product.as_deref().map(Product::new));

                let bmc_cred_data = match expected {
                    Some(v) => {
                        tracing::info!(
                            %bmc_ip_address,
                            %bmc_mac_address,
                            expected_entity = v.name(),
                            "Found an expected entity"
                        );
                        v.bmc_credentials_data()
                    }
                    None => {
                        tracing::info!(%bmc_ip_address, %bmc_mac_address, %vendor, "No expected machine found, could be a BlueField");
                        match vendor {
                            RedfishVendor::NvidiaDpu => {
                                // This machine is a DPU. Use the per-model factory default credential
                                // (looked up above from vault, with hardcoded fallback).
                                let Credentials::UsernamePassword {
                                    ref username,
                                    ref password,
                                } = *dpu_factory_creds.as_ref().unwrap();
                                BmcCredentialsData {
                                    username,
                                    password,
                                    retain_credentials: false,
                                }
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

                let bmc_credentials = match self
                    .set_sitewide_bmc_root_password(
                        bmc_ip_address,
                        bmc_mac_address,
                        vendor,
                        bmc_cred_data,
                    )
                    .await
                {
                    Ok(bmc_credentials) => bmc_credentials,
                    Err(
                        EndpointExplorationError::Unauthorized { .. }
                        | EndpointExplorationError::MissingCredentials { .. },
                    ) => {
                        self.try_sitewide_bmc_root_credentials(
                            bmc_ip_address,
                            bmc_mac_address,
                            bmc_cred_data.username,
                        )
                        .await?
                    }
                    Err(e) => return Err(e),
                };

                if is_bf4_dpu {
                    self.rotate_dpu_service_password_from_factory_defaults(
                        bmc_ip_address,
                        bmc_mac_address,
                        &bmc_credentials,
                    )
                    .await?;
                }

                self.generate_exploration_report(
                    bmc_ip_address,
                    bmc_credentials,
                    boot_interface,
                    Some(vendor),
                )
                .await?
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
                            "NVOS admin credentials already exist in vault"
                        );
                    }
                    Err(e) => {
                        tracing::info!(
                            %bmc_ip_address,
                            %bmc_mac_address,
                            error = %e,
                            "Failed to load NVOS admin credentials; attempting credential setup",
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
}

#[async_trait::async_trait]
impl AuthenticatedBmc for AuthenticatedBmcClient {
    async fn have_credentials(&self, interface: &MachineInterfaceSnapshot) -> bool {
        self.get_bmc_root_credentials(interface.mac_address)
            .await
            .is_ok()
    }

    async fn redfish_reset_bmc(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        reset_type: Option<libredfish::ManagerResetType>,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.redfish_client
                    .reset_bmc(bmc_ip_address, credentials, reset_type)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for BMC reset",
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
            .bmc_cold_reset(
                SocketAddr::new(bmc_ip_address.ip(), carbide_ipmi::DEFAULT_IPMI_PORT),
                &credential_key,
            )
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
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for fetching live power state",
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
                self.redfish_client
                    .power(bmc_ip_address, credentials, action)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for power control",
                );
                Err(e)
            }
        }
    }

    async fn redfish_chassis_reset(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        chassis_id: &str,
        action: libredfish::SystemPowerControl,
    ) -> Result<(), EndpointExplorationError> {
        let credentials = self.get_bmc_root_credentials(interface.mac_address).await?;
        self.redfish_client
            .chassis_reset(bmc_ip_address, credentials, chassis_id, action)
            .await
    }

    async fn disable_secure_boot(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.redfish_client
                    .disable_secure_boot(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for disabling secure boot",
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
            Ok(credentials) => {
                self.redfish_client
                    .lockdown(bmc_ip_address, credentials, action)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for changing lockdown state",
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
            Ok(credentials) => {
                self.redfish_client
                    .lockdown_status(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for checking lockdown status",
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
            Ok(credentials) => {
                self.redfish_client
                    .enable_infinite_boot(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for enabling infinite boot",
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
                self.redfish_client
                    .is_infinite_boot_enabled(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for checking infinite boot status",
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
                self.redfish_client
                    .machine_setup(bmc_ip_address, credentials, boot_interface)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for machine setup",
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
                self.redfish_client
                    .set_boot_order_dpu_first(bmc_ip_address, credentials, boot_interface)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for configuring boot order",
                );
                Err(e)
            }
        }
    }

    async fn set_nic_mode(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        mode: BlueFieldOperatingMode,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        match self.get_bmc_root_credentials(bmc_mac_address).await {
            Ok(credentials) => {
                self.redfish_client
                    .set_nic_mode(bmc_ip_address, credentials, mode.into_libredfish())
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for setting NIC mode",
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
            Ok(credentials) => {
                self.redfish_client
                    .is_viking(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for checking BMC hardware type",
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
            Ok(credentials) => {
                self.redfish_client
                    .clear_nvram(bmc_ip_address, credentials)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for clearing NVRAM",
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
                self.redfish_client
                    .create_bmc_user(bmc_ip_address, credentials, username, password, role_id)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for creating BMC user",
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
                self.redfish_client
                    .delete_bmc_user(bmc_ip_address, credentials, username)
                    .await
            }
            Err(e) => {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for deleting BMC user",
                );
                Err(e)
            }
        }
    }

    async fn set_bmc_root_password(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        new_password: &str,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        let current_credentials = self
            .get_bmc_root_credentials(bmc_mac_address)
            .await
            .inspect_err(|e| {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for setting BMC root password",
                );
            })?;

        // Resolve the dispatch vendor `set_bmc_root_password` branches on using
        // the current credentials, then set the new password on the device.
        let vendor = self
            .redfish_client
            .probe_bmc_vendor(bmc_ip_address, current_credentials.clone())
            .await?;
        let new_credentials = self
            .set_bmc_root_password(
                bmc_ip_address,
                vendor,
                current_credentials,
                new_password.to_string(),
            )
            .await?;

        // Persist the new per-device credential so NICo can still reach the BMC.
        // Deliberately does NOT record rotation convergence (unlike
        // `set_bmc_root_credentials`): this is an out-of-band set, so the
        // credential-rotation engine will reassert the site-wide password on
        // its next pass rather than treating this device as converged.
        self.credential_client
            .set_bmc_root_credentials(bmc_mac_address, &new_credentials)
            .await?;

        Ok(())
    }

    async fn probe_bmc_vendor(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<RedfishVendor, EndpointExplorationError> {
        let bmc_mac_address = interface.mac_address;

        let credentials = self
            .get_bmc_root_credentials(bmc_mac_address)
            .await
            .inspect_err(|e| {
                tracing::info!(
                    %bmc_ip_address,
                    %bmc_mac_address,
                    error = %e,
                    "Failed to load BMC root credentials for probing BMC vendor",
                );
            })?;

        self.redfish_client
            .probe_bmc_vendor(bmc_ip_address, credentials)
            .await
    }
}

// This report is temporary. For transition period when we check that
// nv-redfish produces the same reports as libredfish.
fn warn_report_diff(report1: &EndpointExplorationReport, report2: &EndpointExplorationReport) {
    if report1.endpoint_type != report2.endpoint_type {
        tracing::warn!(
            libredfish_endpoint_type = ?report1.endpoint_type,
            nvredfish_endpoint_type = ?report2.endpoint_type,
            "endpoint types are not equal"
        );
    }

    if report1.vendor != report2.vendor {
        tracing::warn!(
            libredfish_vendor = ?report1.vendor,
            nvredfish_vendor = ?report2.vendor,
            "vendors are not equal"
        );
    }

    if report1.managers != report2.managers {
        tracing::warn!(
            libredfish_managers = ?report1.managers,
            nvredfish_managers = ?report2.managers,
            "managers are not equal"
        );
    }

    if report1.systems.len() != report2.systems.len() {
        tracing::warn!(
            libredfish_system_count = report1.systems.len(),
            nvredfish_system_count = report2.systems.len(),
            "reported different number of systems",
        );
    }

    for (s1, s2) in report1.systems.iter().zip(report2.systems.iter()) {
        if s1.id != s2.id {
            tracing::warn!(
                libredfish_system_id = ?s1.id,
                nvredfish_system_id = ?s2.id,
                "system IDs are not equal"
            );
        } else {
            if s1.ethernet_interfaces != s2.ethernet_interfaces {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_ethernet_interfaces = ?s1.ethernet_interfaces,
                    nvredfish_ethernet_interfaces = ?s2.ethernet_interfaces,
                    "system Ethernet interfaces are not equal"
                );
            }

            if s1.manufacturer != s2.manufacturer {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_manufacturer = ?s1.manufacturer,
                    nvredfish_manufacturer = ?s2.manufacturer,
                    "system manufacturers are not equal"
                );
            }

            if s1.model != s2.model {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_model = ?s1.model,
                    nvredfish_model = ?s2.model,
                    "system models are not equal"
                );
            }

            if s1.serial_number != s2.serial_number {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_serial_number = ?s1.serial_number,
                    nvredfish_serial_number = ?s2.serial_number,
                    "system serial numbers are not equal"
                );
            }

            if s1.attributes != s2.attributes {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_attributes = ?s1.attributes,
                    nvredfish_attributes = ?s2.attributes,
                    "system attributes are not equal"
                );
            }

            if s1.pcie_devices != s2.pcie_devices {
                if s1.pcie_devices.len() != s2.pcie_devices.len() {
                    tracing::warn!(
                        system_id = ?s1.id,
                        libredfish_pcie_device_ids = ?s1.pcie_devices
                            .iter()
                            .map(|v| v.id.as_ref())
                            .collect::<Vec<_>>(),
                        nvredfish_pcie_device_ids = ?s2.pcie_devices
                            .iter()
                            .map(|v| v.id.as_ref())
                            .collect::<Vec<_>>(),
                        "system PCIe device counts are not equal",
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
                                    system_id = ?s1.id,
                                    device_id = ?s1dev.id,
                                    libredfish_pcie_device = ?s1dev,
                                    nvredfish_pcie_device = ?s2dev,
                                    "system PCIe devices are not equal",
                                );
                            }
                        } else {
                            tracing::warn!(
                                system_id = ?s1.id,
                                device_id = ?s1dev.id,
                                "system PCIe device is missing from the second report"
                            );
                        }
                    }
                }
            }

            if s1.base_mac != s2.base_mac {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_base_mac_address = ?s1.base_mac,
                    nvredfish_base_mac_address = ?s2.base_mac,
                    "system base MAC addresses are not equal"
                );
            }

            if s1.power_state != s2.power_state {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_power_state = ?s1.power_state,
                    nvredfish_power_state = ?s2.power_state,
                    "system power states are not equal"
                );
            }

            if s1.sku != s2.sku {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_sku = ?s1.sku,
                    nvredfish_sku = ?s2.sku,
                    "system SKUs are not equal"
                );
            }

            if s1.boot_order != s2.boot_order {
                tracing::warn!(
                    system_id = ?s1.id,
                    libredfish_boot_order = ?s1.boot_order,
                    nvredfish_boot_order = ?s2.boot_order,
                    "system boot orders are not equal"
                );
            }
        }
    }

    if report1.chassis.len() != report2.chassis.len() {
        tracing::warn!(
            libredfish_chassis_count = report1.chassis.len(),
            nvredfish_chassis_count = report2.chassis.len(),
            "reported different number of chassis",
        );
    }

    for (c1, c2) in report1.chassis.iter().zip(report2.chassis.iter()) {
        if c1.id != c2.id {
            tracing::warn!(
                libredfish_chassis_id = ?c1.id,
                nvredfish_chassis_id = ?c2.id,
                "chassis IDs are not equal"
            );
        } else if c1 != c2 {
            tracing::warn!(
                chassis_id = ?c1.id,
                libredfish_chassis = ?c1,
                nvredfish_chassis = ?c2,
                "chassis reports are not equal"
            );
        }
    }

    if report1.service.len() != report2.service.len() {
        tracing::warn!(
            libredfish_service_count = report1.service.len(),
            nvredfish_service_count = report2.service.len(),
            "reported different number of service",
        );
    }

    for (s1, s2) in report1.service.iter().zip(report2.service.iter()) {
        if s1.id != s2.id {
            tracing::warn!(
                libredfish_service_id = ?s1.id,
                nvredfish_service_id = ?s2.id,
                "service IDs are not equal"
            );
        } else {
            if s1.inventories.len() != s2.inventories.len() {
                tracing::warn!(
                    service_id = ?s1.id,
                    libredfish_service = ?s1,
                    nvredfish_service = ?s2,
                    "service reports are not equal"
                );
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
                    || i1.release_date.as_ref().filter(|&v| v != "00:00:00Z")
                        != i2.release_date.as_ref().filter(|&v| v != "00:00:00Z")
                {
                    tracing::warn!(
                        service_id = ?s1.id,
                        libredfish_inventory = ?i1,
                        nvredfish_inventory = ?i2,
                        "service inventories are not equal"
                    );
                }
            }
        }
    }

    if report1.machine_setup_status.is_some() != report2.machine_setup_status.is_some() {
        tracing::warn!(
            libredfish_machine_setup_status = ?report1.machine_setup_status,
            nvredfish_machine_setup_status = ?report2.machine_setup_status,
            "machine setup statuses are not equal",
        );
    } else if let Some(r1) = &report1.machine_setup_status
        && let Some(r2) = &report2.machine_setup_status
    {
        // Both backends should retain the same logical target. Keep the target
        // in this comparison so future backend changes cannot hide a mismatch.
        if r1.is_done != r2.is_done || r1.evaluated_boot_interface != r2.evaluated_boot_interface {
            tracing::warn!(
                libredfish_machine_setup_status = ?r1,
                nvredfish_machine_setup_status = ?r2,
                "machine setup statuses are not equal"
            );
        }

        let mut sst1_idx = (0..r1.diffs.len()).collect::<Vec<_>>();
        sst1_idx.sort_by_key(|i| &r1.diffs[*i].key);
        let mut sst2_idx = (0..r2.diffs.len()).collect::<Vec<_>>();
        sst2_idx.sort_by_key(|i| &r2.diffs[*i].key);
        if sst1_idx.len() != sst2_idx.len() {
            tracing::warn!(
                libredfish_machine_setup_diffs = ?r1.diffs,
                nvredfish_machine_setup_diffs = ?r2.diffs,
                "machine setup status differences are not equal"
            );
        } else {
            for (i1, i2) in sst1_idx.into_iter().zip(sst2_idx) {
                let d1 = &r1.diffs[i1];
                let d2 = &r2.diffs[i2];
                if d1 != d2 {
                    tracing::warn!(
                        libredfish_machine_setup_diff = ?d1,
                        nvredfish_machine_setup_diff = ?d2,
                        "machine setup status differences are not equal"
                    );
                }
            }
        }
    }

    if report1.secure_boot_status != report2.secure_boot_status {
        tracing::warn!(
            libredfish_secure_boot_status = ?report1.secure_boot_status,
            nvredfish_secure_boot_status = ?report2.secure_boot_status,
            "secure boot statuses are not equal",
        );
    }

    if report1.lockdown_status != report2.lockdown_status {
        tracing::warn!(
            libredfish_lockdown_status = ?report1.lockdown_status,
            nvredfish_lockdown_status = ?report2.lockdown_status,
            "lockdown statuses are not equal",
        );
    }

    if report1.power_shelf_id != report2.power_shelf_id {
        tracing::warn!(
            libredfish_power_shelf_id = ?report1.power_shelf_id,
            nvredfish_power_shelf_id = ?report2.power_shelf_id,
            "power shelf IDs are not equal"
        )
    }

    if report1.switch_id != report2.switch_id {
        tracing::warn!(
            libredfish_switch_id = ?report1.switch_id,
            nvredfish_switch_id = ?report2.switch_id,
            "switch IDs are not equal"
        )
    }

    if report1.physical_slot_number != report2.physical_slot_number {
        tracing::warn!(
            libredfish_physical_slot_number = ?report1.physical_slot_number,
            nvredfish_physical_slot_number = ?report2.physical_slot_number,
            "physical slot numbers are not equal"
        )
    }

    if report1.compute_tray_index != report2.compute_tray_index {
        tracing::warn!(
            libredfish_compute_tray_index = ?report1.compute_tray_index,
            nvredfish_compute_tray_index = ?report2.compute_tray_index,
            "compute tray indexes are not equal"
        )
    }

    if report1.topology_id != report2.topology_id {
        tracing::warn!(
            libredfish_topology_id = ?report1.topology_id,
            nvredfish_topology_id = ?report2.topology_id,
            "topology IDs are not equal"
        )
    }

    if report1.revision_id != report2.revision_id {
        tracing::warn!(
            libredfish_revision_id = ?report1.revision_id,
            nvredfish_revision_id = ?report2.revision_id,
            "revision IDs are not equal"
        )
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use arc_swap::ArcSwap;
    use axum::Router;
    use bmc_mock::{CombinedServer, ListenerOrAddress};
    use carbide_instrument::Outcome;
    use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};
    use carbide_redfish::libredfish::test_support::{
        RedfishSim, RedfishSimAuthAttempt, RedfishSimBootInterfaceRef,
    };
    use carbide_redfish::nv_redfish::NvRedfishClientPool;
    use carbide_secrets::credentials::{
        BmcCredentialType, CredentialKey, CredentialReader, CredentialWriter,
    };
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async, value_scenarios};
    use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
    use model::machine_boot_interface::{MachineBootInterface, MachineBootInterfaceTarget};
    use model::site_explorer::MachineSetupStatus;

    use super::*;

    fn report_with_evaluated_target(
        evaluated_boot_interface: Option<MachineBootInterfaceTarget>,
    ) -> EndpointExplorationReport {
        EndpointExplorationReport {
            machine_setup_status: Some(MachineSetupStatus {
                is_done: true,
                diffs: Vec::new(),
                evaluated_boot_interface,
            }),
            ..Default::default()
        }
    }

    async fn explore_router_in_default_mode(router: Router) -> EndpointExplorationReport {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bmc_ip_address = listener.local_addr().unwrap();
        let _server = CombinedServer::run_router(
            "nv-redfish-port-test",
            router,
            Some(ListenerOrAddress::Listener(listener)),
            bmc_mock::tls::server_config(None::<&str>).unwrap(),
        );

        let mode = crate::config::SiteExplorerConfig::default_explore_mode();
        assert_eq!(mode, SiteExplorerExploreMode::NvRedfish);
        let proxy_address = Arc::new(ArcSwap::new(Arc::new(None)));
        let bmc_client = Arc::new(AuthenticatedBmcClient::new(
            Arc::new(RedfishSim::default()),
            Arc::new(NvRedfishClientPool::new(proxy_address)),
            carbide_ipmi::test_support(),
            Arc::new(TestCredentialManager::default()),
        ));
        let explorer =
            BmcEndpointExplorer::new(bmc_client, Arc::new(AtomicBool::new(false)), mode, None);

        explorer
            .generate_exploration_report(
                bmc_ip_address,
                Credentials::new("root", "password"),
                None,
                None,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn default_nvredfish_mode_does_not_apply_lenovo_fallback_to_generic_ami() {
        let (router, _state) = bmc_mock::test_support::
            generic_ami_router_with_network_adapter_port_and_disabled_system_mac(
                serde_json::json!({
                "@odata.id": "/redfish/v1/Chassis/Self/NetworkAdapters/1/Ports/1",
                "@odata.type": "#Port.v1_6_0.Port",
                "Id": "1",
                "Name": "Port 1",
                "Oem": {
                    "Lenovo": { "PhysicalPortMacAddress": "946DAE53CB9B" }
                }
                }),
            );
        let report = explore_router_in_default_mode(router).await;

        assert!(report.all_mac_addresses().is_empty());
        assert!(
            report.chassis[0].network_adapters[0]
                .port_mac_addresses
                .is_empty()
        );
        assert!(
            report.systems[0]
                .ethernet_interfaces
                .iter()
                .any(|interface| {
                    interface.id.as_deref() == Some("disabled") && interface.mac_address.is_none()
                })
        );
    }

    #[tokio::test]
    async fn default_nvredfish_mode_discovers_lenovo_port_mac_with_partial_system_inventory() {
        let report = explore_router_in_default_mode(
            bmc_mock::test_support::lenovo_xcc_router_with_partial_system_network_inventory(),
        )
        .await;
        let port_mac_address = "94:6d:ae:53:cb:9b".parse().unwrap();
        let adapter = report
            .chassis
            .iter()
            .flat_map(|chassis| &chassis.network_adapters)
            .find(|adapter| adapter.id == "slot-15")
            .expect("ConnectX-7 adapter must be explored");

        assert_eq!(adapter.port_mac_addresses, vec![port_mac_address]);
        assert_eq!(report.systems[0].ethernet_interfaces.len(), 5);
        assert!(
            report.systems[0]
                .ethernet_interfaces
                .iter()
                .all(|interface| {
                    interface.interface_enabled == Some(true) && interface.mac_address.is_some()
                }),
            "the System interface inventory must remain usable",
        );
        assert!(
            report.systems[0]
                .ethernet_interfaces
                .iter()
                .all(|interface| interface.mac_address != Some(port_mac_address)),
            "adapter Port inventory must not be synthesized into the System interface collection",
        );
        assert_eq!(report.find_interface_id_for_mac(port_mac_address), None);
    }

    #[test]
    fn report_comparison_includes_the_evaluated_boot_interface() {
        let mac_address = MacAddress::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01]);
        let pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.7-1-1".to_string(),
        });
        let mac_only = MachineBootInterfaceTarget::MacOnly(mac_address);

        value_scenarios!(run = |(libredfish_target, nvredfish_target)| {
            let libredfish_report = report_with_evaluated_target(libredfish_target);
            let nvredfish_report = report_with_evaluated_target(nvredfish_target);
            capture_logs(|| warn_report_diff(&libredfish_report, &nvredfish_report))
                .into_iter()
                .map(|log| log.message)
                .collect::<Vec<_>>()
        };
            "same evaluated pair does not warn" {
                (Some(pair.clone()), Some(pair)) => Vec::<String>::new(),
            }

            "pair and MAC-only targets warn" {
                (Some(MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address,
                    interface_id: "NIC.Slot.7-1-1".to_string(),
                })), Some(mac_only)) => vec!["machine setup statuses are not equal".to_string()],
            }

            "two targetless statuses do not warn" {
                (None, None) => Vec::<String>::new(),
            }

            "pair and targetless statuses warn" {
                (Some(MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address,
                    interface_id: "NIC.Slot.7-1-1".to_string(),
                })), None) => vec!["machine setup statuses are not equal".to_string()],
            }
        );
    }

    async fn explore_after_credential_bootstrap(
        boot_interface: Option<BootInterfaceTarget>,
    ) -> Result<Vec<Option<RedfishSimBootInterfaceRef>>, String> {
        let sim = Arc::new(RedfishSim::default());
        let proxy_address = Arc::new(ArcSwap::new(Arc::new(None)));
        let bmc_client = Arc::new(AuthenticatedBmcClient::new(
            sim.clone(),
            Arc::new(NvRedfishClientPool::new(proxy_address)),
            carbide_ipmi::test_support(),
            Arc::new(TestCredentialManager::default()),
        ));
        let explorer = BmcEndpointExplorer::new(
            bmc_client,
            Arc::new(AtomicBool::new(false)),
            SiteExplorerExploreMode::LibRedfish,
            None,
        );
        let bmc_ip_address: SocketAddr = "127.0.0.1:443".parse().expect("valid test BMC address");
        let bmc_mac_address: MacAddress = "02:00:00:00:00:01".parse().expect("valid test BMC MAC");
        let interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);
        let expected = ExpectedEntity::Machine(ExpectedMachine {
            id: None,
            bmc_mac_address,
            data: ExpectedMachineData {
                bmc_username: "root".to_string(),
                bmc_password: "factory-password".to_string(),
                serial_number: "credential-bootstrap-host".to_string(),
                bmc_retain_credentials: Some(true),
                ..Default::default()
            },
        });

        explorer
            .explore_endpoint(
                bmc_ip_address,
                &interface,
                Some(&expected),
                None,
                boot_interface.as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;

        Ok(sim.machine_setup_status_targets(&bmc_ip_address.ip().to_string()))
    }

    #[tokio::test]
    async fn credential_bootstrap_preserves_the_evaluated_boot_interface() {
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
                    expect: Yields(vec![Some(RedfishSimBootInterfaceRef::Pair {
                        mac_address,
                        interface_id: boot_interface.interface_id,
                    })]),
                },
                Case {
                    scenario: "legacy MAC only",
                    input: Some(BootInterfaceTarget::MacOnly(mac_address)),
                    expect: Yields(vec![Some(RedfishSimBootInterfaceRef::Mac(mac_address))]),
                },
                Case {
                    scenario: "no boot interface",
                    input: None,
                    expect: Yields(vec![None]),
                },
            ],
            explore_after_credential_bootstrap,
        )
        .await;
    }

    struct DpuGeneration {
        product: &'static str,
        username: &'static str,
    }

    struct ReingestionFixture {
        sim: Arc<RedfishSim>,
        credential_manager: Arc<TestCredentialManager>,
        explorer: BmcEndpointExplorer,
        bmc_ip_address: SocketAddr,
        interface: MachineInterfaceSnapshot,
        sitewide_credential_key: CredentialKey,
        per_bmc_credential_key: CredentialKey,
        auth_attempt_count: usize,
        username: &'static str,
    }

    const DPU_FACTORY_PASSWORD: &str = "0penBmc";
    const SITEWIDE_PASSWORD: &str = "sitewide-password";

    async fn prepare_reingestion_fixture(
        generation: DpuGeneration,
    ) -> Result<ReingestionFixture, String> {
        // Start with model-specific factory credentials and no MAC-specific credential record.
        // This makes the first exploration follow the initial-ingestion path.
        let sim = Arc::new(RedfishSim::default());
        sim.set_service_root_product(Some(generation.product.to_string()));
        sim.set_enforce_auth(true);
        sim.seed_user(generation.username, DPU_FACTORY_PASSWORD);
        // BF4 rotates both administrative accounts, so seed service to let ingestion finish.
        sim.seed_user("service", "factory-service-password");

        // Store the site-wide password in Vault so initial ingestion can install it
        // on the DPU and create its MAC-specific credential record.
        let credential_manager = Arc::new(TestCredentialManager::default());
        let sitewide_credential_key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRoot,
        };
        credential_manager
            .set_credentials(
                &sitewide_credential_key,
                &Credentials::new("", SITEWIDE_PASSWORD),
            )
            .await
            .expect("seed site-wide BMC credentials");

        let proxy_address = Arc::new(ArcSwap::new(Arc::new(None)));
        let bmc_client = Arc::new(AuthenticatedBmcClient::new(
            sim.clone(),
            Arc::new(NvRedfishClientPool::new(proxy_address)),
            carbide_ipmi::test_support(),
            credential_manager.clone(),
        ));
        let explorer = BmcEndpointExplorer::new(
            bmc_client,
            Arc::new(AtomicBool::new(false)),
            SiteExplorerExploreMode::LibRedfish,
            None,
        );
        let bmc_ip_address: SocketAddr = "127.0.0.1:443".parse().expect("valid test BMC address");
        let bmc_mac_address: MacAddress = "02:00:00:00:00:01".parse().expect("valid test BMC MAC");
        let interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);
        let per_bmc_credential_key = get_bmc_root_credential_key(bmc_mac_address);

        // Run initial ingestion to establish the state that exists before deletion.
        // It must rotate the DPU password and record the site-wide credential by MAC address.
        explorer
            .explore_endpoint(bmc_ip_address, &interface, None, None, None)
            .await
            .map_err(|error| format!("initial ingestion failed: {error}"))?;
        assert_eq!(
            credential_manager
                .get_credentials(&per_bmc_credential_key)
                .await
                .map_err(|error| error.to_string())?,
            Some(Credentials::new(generation.username, SITEWIDE_PASSWORD)),
            "initial ingestion must record the site-wide credential by MAC address"
        );
        assert_eq!(
            sim.user_password(generation.username).as_deref(),
            Some(SITEWIDE_PASSWORD),
            "initial ingestion must rotate the hardware off its factory password"
        );

        // Record the current attempt count so later assertions inspect only re-ingestion.
        let auth_attempt_count = sim.auth_attempts().len();
        // Machine deletion is outside Site Explorer, but it removes this MAC-specific
        // record. Delete only that record to reproduce the state passed to re-ingestion.
        // Keep the global site-wide record because it survives deletion and enables fallback.
        credential_manager
            .delete_credentials(&per_bmc_credential_key)
            .await
            .expect("delete the MAC-specific credential record with the machine entry");

        Ok(ReingestionFixture {
            sim,
            credential_manager,
            explorer,
            bmc_ip_address,
            interface,
            sitewide_credential_key,
            per_bmc_credential_key,
            auth_attempt_count,
            username: generation.username,
        })
    }

    async fn reingest_with_sitewide_password(
        generation: DpuGeneration,
    ) -> Result<Credentials, String> {
        let fixture = prepare_reingestion_fixture(generation).await?;

        // Keep the site-wide password on the DPU while its MAC-specific record is absent.
        // This tests recovery through the site-wide credential after factory auth fails.
        fixture
            .explorer
            .explore_endpoint(fixture.bmc_ip_address, &fixture.interface, None, None, None)
            .await
            .map_err(|error| format!("re-ingestion failed: {error}"))?;

        let auth_attempts = fixture.sim.auth_attempts();
        let auth_attempts = &auth_attempts[fixture.auth_attempt_count..];
        assert_eq!(
            auth_attempts.first(),
            Some(&RedfishSimAuthAttempt {
                credentials: Credentials::new(fixture.username, DPU_FACTORY_PASSWORD),
                authorized: false,
            }),
            "factory-default credentials must be attempted and rejected first"
        );
        assert_eq!(
            auth_attempts.get(1),
            Some(&RedfishSimAuthAttempt {
                credentials: Credentials::new(fixture.username, SITEWIDE_PASSWORD),
                authorized: true,
            }),
            "site-wide credentials must authenticate after the factory default fails"
        );

        fixture
            .credential_manager
            .get_credentials(&fixture.per_bmc_credential_key)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "re-ingestion did not restore the MAC-specific credential record".to_string()
            })
    }

    // Authentication failures are throttled, so pause time to test fallback without waiting.
    #[tokio::test(start_paused = true)]
    async fn reingested_dpus_restore_missing_bmc_credentials_from_sitewide_password() {
        // Ingestion emits the rotation metric, so hold its test lock to isolate metric counts.
        let _metrics_window = MetricsCapture::start();
        // BF3 uses root and BF4 uses admin, so run the same recovery check for both.
        check_cases_async(
            [
                Case {
                    scenario: "BlueField-3 restores the root credential",
                    input: DpuGeneration {
                        product: "BlueField-3 DPU",
                        username: "root",
                    },
                    expect: Yields(Credentials::new("root", SITEWIDE_PASSWORD)),
                },
                Case {
                    scenario: "BlueField-4 restores the admin credential",
                    input: DpuGeneration {
                        product: "BlueField-4 DPU",
                        username: "admin",
                    },
                    expect: Yields(Credentials::new("admin", SITEWIDE_PASSWORD)),
                },
            ],
            reingest_with_sitewide_password,
        )
        .await;
    }

    // Authentication failures are throttled, so pause time to test rejection without waiting.
    #[tokio::test(start_paused = true)]
    async fn reingested_dpu_rejects_stale_sitewide_password() {
        let _metrics_window = MetricsCapture::start();
        let fixture = prepare_reingestion_fixture(DpuGeneration {
            product: "BlueField-4 DPU",
            username: "admin",
        })
        .await
        .expect("prepare a previously ingested DPU");

        // Change the site-wide password in Vault without changing the DPU password.
        // This tests that failure of primary and fallback authentication is a hard failure.
        let stale_password = "stale-sitewide-password";
        fixture
            .credential_manager
            .set_credentials(
                &fixture.sitewide_credential_key,
                &Credentials::new("", stale_password),
            )
            .await
            .expect("replace the site-wide credential with a stale password");

        let exploration = fixture
            .explorer
            .explore_endpoint(fixture.bmc_ip_address, &fixture.interface, None, None, None)
            .await;
        assert!(exploration.is_err(), "stale credentials must be rejected");

        let auth_attempts = fixture.sim.auth_attempts();
        let auth_attempts = &auth_attempts[fixture.auth_attempt_count..];
        assert_eq!(
            auth_attempts.first(),
            Some(&RedfishSimAuthAttempt {
                credentials: Credentials::new(fixture.username, DPU_FACTORY_PASSWORD),
                authorized: false,
            }),
            "factory-default credentials must be attempted and rejected first"
        );
        assert_eq!(
            auth_attempts.get(1),
            Some(&RedfishSimAuthAttempt {
                credentials: Credentials::new(fixture.username, stale_password),
                authorized: false,
            }),
            "stale site-wide credentials must be attempted and rejected"
        );
        assert_eq!(
            fixture
                .credential_manager
                .get_credentials(&fixture.per_bmc_credential_key)
                .await
                .expect("read the MAC-specific credential record after failed re-ingestion"),
            None,
            "hard failure must leave the MAC-specific credential record absent"
        );
    }

    /// One emit per rotation attempt writes the INFO log line and moves
    /// carbide_site_explorer_bmc_password_rotations_total, split by outcome.
    #[test]
    fn bmc_password_rotation_counts_both_outcomes() {
        let metrics = MetricsCapture::start();
        let bmc_ip_address: SocketAddr = "10.2.3.4:443".parse().expect("socket address");
        let bmc_mac_address: MacAddress = "aa:bb:cc:dd:ee:ff".parse().expect("mac address");

        let logs = capture_logs(|| {
            carbide_instrument::emit(BmcPasswordRotationFinished {
                outcome: Outcome::Ok,
                bmc_ip_address,
                bmc_mac_address,
                vendor: RedfishVendor::Dell,
                error: String::new(),
            });
            carbide_instrument::emit(BmcPasswordRotationFinished {
                outcome: Outcome::Error,
                bmc_ip_address,
                bmc_mac_address,
                vendor: RedfishVendor::Dell,
                error: "unable to log into the BMC".to_string(),
            });
        });

        assert_eq!(logs.len(), 2);
        for log in &logs {
            assert_eq!(log.level, tracing::Level::INFO);
            assert_eq!(log.message, "BMC root password rotation finished");
        }
        let field = |log: &CapturedLog, name: &str| {
            log.fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(field(&logs[0], "outcome"), Some("ok".to_string()));
        assert_eq!(
            field(&logs[0], "bmc_ip_address"),
            Some("10.2.3.4:443".to_string())
        );
        assert_eq!(field(&logs[1], "outcome"), Some("error".to_string()));
        assert_eq!(
            field(&logs[1], "error"),
            Some("unable to log into the BMC".to_string())
        );

        assert_eq!(
            metrics.counter_delta(
                "carbide_site_explorer_bmc_password_rotations_total",
                &[("outcome", "ok")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_site_explorer_bmc_password_rotations_total",
                &[("outcome", "error")]
            ),
            1.0
        );
    }
}
