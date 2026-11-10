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

//! Reaching a BMC with the CLI credentials: discovery of the
//! [`PlatformIdentity`] selection rules match on, through the same
//! exploration site-explorer runs, fresh logins and reads, and the
//! `bmc-runtime` connection checks drive drivers through.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bmc_platform::{
    ClassifyBmcError, Fetched, IpmiOps, PlatformError, PlatformIdentity, ServiceRootIdentity,
};
use bmc_runtime::{
    BmcRef, ConnectedBmc, ConnectionManager, CredentialLease, CredentialRequest, EndpointIpmiOps,
    RuntimeCredentialProvider,
};
use carbide_redfish::nv_redfish::{NvRedfishClientPool, NvRedfishConnection, RedfishBmc};
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use mac_address::MacAddress;
use nv_redfish::Bmc;
use nv_redfish::bmc_http::BmcCredentials;
use nv_redfish::core::ODataId;
use serde_json::{Map, Value};

/// The MAC address in the validator's credential key. `BmcRef` keys
/// credentials by BMC MAC, but the validator issues the CLI credentials for
/// every key, so the address is never looked up.
const CREDENTIAL_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0];

/// How long discovery may take before it gives up; one read of a large
/// inventory can take most of the two-minute request timeout.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(6 * 60);

/// How long discovery waits for the ServiceRoot to report a vendor or a
/// product, and how often it looks again meanwhile.
const UNSETTLED_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UNSETTLED_INTERVAL: Duration = Duration::from_secs(10);

/// How long exploration waits between retries of a read the BMC refused.
const EXPLORE_RETRY: Duration = Duration::from_secs(1);

/// The name the validator requests credentials under.
const CALLER: &str = "bmc-validate";

pub(crate) fn credential_key() -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: MacAddress::new(CREDENTIAL_MAC),
        },
    }
}

/// One BMC, reached as one account.
#[derive(Clone)]
pub(crate) struct Target {
    pool: Arc<NvRedfishClientPool>,
    address: SocketAddr,
    username: String,
    password: String,
}

impl Target {
    pub(crate) fn new(address: SocketAddr, username: &str, password: &str) -> Self {
        Self {
            pool: carbide_redfish::nv_redfish::new_pool(Arc::new(ArcSwap::from_pointee(None))),
            address,
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    /// The same BMC reached as another account.
    pub(crate) fn as_account(&self, username: &str, password: &str) -> Self {
        Self {
            username: username.to_string(),
            password: password.to_string(),
            ..self.clone()
        }
    }

    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }

    fn credentials(&self) -> BmcCredentials {
        BmcCredentials::new(self.username.clone(), self.password.clone())
    }

    /// A connection over a ServiceRoot read now, with a new transport, so
    /// nothing it reads was cached before an earlier change.
    pub(crate) async fn fresh_connection(&self) -> Result<NvRedfishConnection, PlatformError> {
        self.pool.invalidate_service_roots_for_bmc(self.address);
        self.pool
            .connection_with_bmc_credentials(self.address, self.credentials())
            .await
            .map_err(PlatformError::from_redfish)
    }

    /// Reads an authenticated resource over a fresh connection; a factory
    /// BMC refuses it with `PasswordChangeRequired`.
    pub(crate) async fn log_in(&self) -> Result<(), PlatformError> {
        self.fresh_connection()
            .await?
            .bmc
            .get::<Fetched<Map<String, Value>>>(&ODataId::from("/redfish/v1/Managers".to_string()))
            .await
            .map(drop)
            .map_err(ClassifyBmcError::classify)
    }

    /// The ServiceRoot part of the identity, the only part a factory BMC
    /// serves.
    pub(crate) async fn service_root_identity(&self) -> Result<ServiceRootIdentity, String> {
        let connection = self
            .fresh_connection()
            .await
            .map_err(|error| format!("connecting: {error}"))?;
        Ok(bmc_explorer::service_root_identity(
            &connection.service_root,
        ))
    }

    pub(crate) async fn discover(&self) -> Result<PlatformIdentity, String> {
        tokio::time::timeout(DISCOVERY_TIMEOUT, async {
            let connection = self
                .fresh_connection()
                .await
                .map_err(|error| format!("connecting: {error}"))?;
            bmc_explorer::nv_generate_exploration_report(connection.service_root, &explore_config())
                .await
                .map(|report| bmc_runtime::platform_identity(&report))
                .map_err(|error| format!("exploring: {error}"))
        })
        .await
        .map_err(|_| format!("discovery did not finish within {DISCOVERY_TIMEOUT:?}"))?
    }

    /// Discovers the BMC, discovering again while its ServiceRoot reports
    /// neither a vendor nor a product, as one still starting can, and calling
    /// `waiting` before each wait. Gives up waiting after
    /// `UNSETTLED_TIMEOUT`, since power shelves never report either, and
    /// returns whether the ServiceRoot reported one.
    pub(crate) async fn discover_settled(
        &self,
        mut waiting: impl FnMut(&str),
    ) -> Result<(PlatformIdentity, bool), String> {
        let deadline = Instant::now() + UNSETTLED_TIMEOUT;
        loop {
            let identity = self.discover().await?;
            let root = &identity.service_root;
            let settled = root.vendor.is_some() || root.product.is_some();
            if settled || Instant::now() + UNSETTLED_INTERVAL > deadline {
                return Ok((identity, settled));
            }
            waiting(&format!(
                "the ServiceRoot reports neither a vendor nor a product, as a BMC still \
                 starting can; discovering again every {}s for up to {}s",
                UNSETTLED_INTERVAL.as_secs(),
                UNSETTLED_TIMEOUT.as_secs()
            ));
            tokio::time::sleep(UNSETTLED_INTERVAL).await;
        }
    }

    /// Connects through `bmc-runtime` with this account, attaching
    /// `ipmitool` with the same account for drivers that restart or reset
    /// over IPMI; it runs only when a driver calls it.
    pub(crate) async fn connect(
        &self,
        endpoint: BmcRef,
    ) -> Result<ConnectedBmc<RedfishBmc>, String> {
        ConnectionManager::new(
            self.pool.clone(),
            Arc::new(StaticCredentials(self.credentials())),
            CALLER.to_string(),
        )
        .map_err(|error| error.to_string())?
        .connect(endpoint, Some(self.ipmi()))
        .await
        .map_err(|error| format!("connecting through bmc-runtime: {error}"))
    }

    fn ipmi(&self) -> Arc<dyn IpmiOps> {
        let tool = carbide_ipmi::tool(
            Arc::new(StaticCredentialReader(Credentials::new(
                &self.username,
                &self.password,
            ))),
            None,
        );
        // The machine id only labels ipmitool errors; the validator has no machine record.
        let machine_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0; 32],
            MachineType::Host,
        );
        Arc::new(EndpointIpmiOps::new(
            tool,
            machine_id,
            SocketAddr::new(self.address.ip(), carbide_ipmi::DEFAULT_IPMI_PORT),
            credential_key(),
        ))
    }
}

/// Explores as site exploration does; no boot interface is evaluated, since
/// the identity does not depend on one.
fn explore_config() -> bmc_explorer::Config<'static, RedfishBmc> {
    bmc_explorer::Config {
        boot_interface_mac: None,
        error_classifier: &bmc_explorer::nv_redfish_error_class,
        retry_timeout: EXPLORE_RETRY,
        explore_drive_firmware: false,
    }
}

/// Issues the CLI credentials to `bmc-runtime` for every request.
struct StaticCredentials(BmcCredentials);

#[async_trait]
impl RuntimeCredentialProvider for StaticCredentials {
    async fn issue(&self, _request: &CredentialRequest) -> Result<CredentialLease, PlatformError> {
        Ok(CredentialLease::new(self.0.clone()))
    }
}

/// Hands the CLI credentials to `ipmitool` for every key.
struct StaticCredentialReader(Credentials);

#[async_trait]
impl CredentialReader for StaticCredentialReader {
    async fn get_credentials(
        &self,
        _key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        Ok(Some(self.0.clone()))
    }
}
