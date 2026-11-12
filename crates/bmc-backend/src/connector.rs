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

use bmc_runtime::{DriverConnector, ExploredConnectError};
use carbide_redfish::libredfish::{RedfishClientCreationError, RedfishClientPool};
use carbide_utils::redfish::BmcAccessInfo;
use model::site_explorer::EndpointExplorationReport;

use crate::BackendBmc;

/// Connects to BMCs with their stored credentials on the configured backend.
#[derive(Clone)]
pub enum BmcConnector {
    Libredfish(Arc<dyn RedfishClientPool>),
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

    /// Connects to the BMC at `access`. Only the drivers call `report` for the
    /// exploration report they select by, so libredfish never loads it.
    pub async fn connect<E, Fut>(
        &self,
        access: &BmcAccessInfo,
        report: impl FnOnce() -> Fut,
    ) -> Result<BackendBmc, BmcConnectError<E>>
    where
        Fut: Future<Output = Result<Option<EndpointExplorationReport>, E>>,
    {
        match self {
            Self::Libredfish(pool) => pool
                .client_by_info(access)
                .await
                .map(BackendBmc::from)
                .map_err(BmcConnectError::Libredfish),
            Self::Drivers(drivers) => {
                let report = report().await.map_err(BmcConnectError::Report)?;
                drivers
                    .connect(access, report.as_ref())
                    .await
                    .map(BackendBmc::from)
                    .map_err(BmcConnectError::Drivers)
            }
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
