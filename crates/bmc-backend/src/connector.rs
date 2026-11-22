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

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bmc_runtime::{DriverConnector, ExploredConnectError};
use carbide_redfish::libredfish::{
    BmcCredentialOps, RedfishAuth, RedfishClientCreationError, RedfishClientPool,
};
use carbide_secrets::credentials::Credentials;
use carbide_utils::redfish::BmcAccessInfo;
use model::site_explorer::EndpointExplorationReport;

use crate::BackendBmc;
use crate::accounts::{BackendAccountBmc, LibredfishAccounts};

/// Connects to BMCs with their stored credentials on the configured backend.
#[derive(Clone)]
pub enum BmcConnector {
    /// Libredfish using a single pool for both routed and direct traffic (tests
    /// and unproxied deployments).
    Libredfish(Arc<dyn RedfishClientPool>),
    /// Libredfish with separate routed (possibly proxied) and direct pools.
    LibredfishSplit {
        routed: Arc<dyn RedfishClientPool>,
        direct: Arc<dyn RedfishClientPool>,
    },
    Drivers(Arc<DriverConnector>),
}

impl BmcConnector {
    /// The drivers when the site configured them, otherwise libredfish
    /// clients from `pool`.
    pub fn new(pool: Arc<dyn RedfishClientPool>, drivers: Option<Arc<DriverConnector>>) -> Self {
        match drivers {
            Some(drivers) => Self::Drivers(drivers),
            None => Self::Libredfish(pool),
        }
    }

    /// The drivers when the site configured them, otherwise libredfish using
    /// `routed` for stored-credential traffic and the sealed direct pool
    /// `direct` for explicit credentials that must bypass `nico-bmc-proxy`.
    pub fn new_with_direct(
        routed: Arc<dyn RedfishClientPool>,
        direct: Arc<dyn BmcCredentialOps>,
        drivers: Option<Arc<DriverConnector>>,
    ) -> Self {
        match drivers {
            Some(drivers) => Self::Drivers(drivers),
            None => Self::LibredfishSplit {
                routed,
                direct: direct as Arc<dyn RedfishClientPool>,
            },
        }
    }

    /// Connects to the BMC at `access`. Only the drivers call `report` for the
    /// exploration report they select by, so libredfish never loads it.
    pub fn connect<E, Fut>(
        &self,
        access: &BmcAccessInfo,
        report: impl FnOnce() -> Fut,
    ) -> impl Future<Output = Result<BackendBmc, BmcConnectError<E>>>
    where
        Fut: Future<Output = Result<EndpointExplorationReport, E>>,
    {
        self.connect_with_policy(access, report, false)
    }

    /// Connect for a durable mutation without application-level mutation retries.
    /// IPMI chassis resets use one bounded attempt; protocol-level HTTP retries remain.
    pub fn connect_without_mutation_retries<E, Fut>(
        &self,
        access: &BmcAccessInfo,
        report: impl FnOnce() -> Fut,
    ) -> impl Future<Output = Result<BackendBmc, BmcConnectError<E>>>
    where
        Fut: Future<Output = Result<EndpointExplorationReport, E>>,
    {
        self.connect_with_policy(access, report, true)
    }

    async fn connect_with_policy<E, Fut>(
        &self,
        access: &BmcAccessInfo,
        report: impl FnOnce() -> Fut,
        single_attempt: bool,
    ) -> Result<BackendBmc, BmcConnectError<E>>
    where
        Fut: Future<Output = Result<EndpointExplorationReport, E>>,
    {
        match self {
            Self::Libredfish(pool) | Self::LibredfishSplit { routed: pool, .. } => {
                let result = if single_attempt {
                    pool.create_client_without_mutation_retries(
                        &access.host,
                        access.port,
                        RedfishAuth::for_bmc_mac(access.mac_address),
                        None,
                    )
                    .await
                } else {
                    pool.client_by_info(access).await
                };
                result
                    .map(BackendBmc::from)
                    .map_err(BmcConnectError::Libredfish)
            }
            Self::Drivers(drivers) => {
                let report = report().await.map_err(BmcConnectError::Report)?;
                let result = if single_attempt {
                    drivers
                        .connect_without_mutation_retries(access, &report)
                        .await
                } else {
                    drivers.connect(access, &report).await
                };
                result
                    .map(BackendBmc::from)
                    .map_err(BmcConnectError::Drivers)
            }
        }
    }

    /// Connects to an explored BMC at `access` with explicit `credentials` for
    /// account operations, always bypassing `nico-bmc-proxy` and selecting
    /// drivers from `report`.
    pub async fn connect_with_credentials(
        &self,
        access: &BmcAccessInfo,
        report: &EndpointExplorationReport,
        credentials: Credentials,
    ) -> Result<BackendAccountBmc, BmcConnectError<Infallible>> {
        match self {
            Self::Libredfish(pool) | Self::LibredfishSplit { direct: pool, .. } => {
                LibredfishAccounts::new(pool.clone(), access, credentials, Some(report))
                    .map(BackendAccountBmc::Libredfish)
                    .map_err(BmcConnectError::Libredfish)
            }
            Self::Drivers(drivers) => drivers
                .connect_with_credentials(access, report, credentials)
                .await
                .map(|bmc| BackendAccountBmc::Drivers(Box::new(bmc)))
                .map_err(BmcConnectError::Drivers),
        }
    }

    /// Connects to an unexplored BMC at `access` with explicit `credentials`
    /// for pre-exploration account operations (such as initial factory
    /// takeover), always bypassing `nico-bmc-proxy`.
    pub async fn connect_unexplored_with_credentials(
        &self,
        access: &BmcAccessInfo,
        credentials: Credentials,
    ) -> Result<BackendAccountBmc, BmcConnectError<Infallible>> {
        match self {
            Self::Libredfish(pool) | Self::LibredfishSplit { direct: pool, .. } => {
                LibredfishAccounts::new(pool.clone(), access, credentials, None)
                    .map(BackendAccountBmc::Libredfish)
                    .map_err(BmcConnectError::Libredfish)
            }
            Self::Drivers(drivers) => drivers
                .connect_unexplored_with_credentials(access, credentials)
                .await
                .map(|bmc| BackendAccountBmc::Drivers(Box::new(bmc)))
                .map_err(BmcConnectError::Drivers),
        }
    }

    /// Evicts cached service roots for `address` after a password change.
    pub fn invalidate_bmc(&self, address: SocketAddr) {
        if let Self::Drivers(drivers) = self {
            drivers.invalidate_bmc(address);
        }
    }
}

/// Failure to connect through a [`BmcConnector`]; `Report` is the caller's
/// failure to produce the exploration report.
#[derive(Debug)]
pub enum BmcConnectError<E> {
    Report(E),
    Libredfish(RedfishClientCreationError),
    Drivers(ExploredConnectError),
}

impl BmcConnectError<Infallible> {
    /// Classifies the connection failure as a [`bmc_platform::PlatformError`]
    /// so callers can branch on authentication or transport errors without
    /// knowing which backend they are on. Mirrors
    /// [`ExploredConnectError::into_platform_error`].
    pub fn into_platform_error(self) -> bmc_platform::PlatformError {
        match self {
            Self::Report(never) => match never {},
            Self::Libredfish(err) => crate::accounts::client_creation_error(err),
            Self::Drivers(err) => err.into_platform_error(),
        }
    }
}
