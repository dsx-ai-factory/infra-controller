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

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use carbide_firmware::{FirmwareConfig, FirmwareConfigSnapshot};
use carbide_redfish::boot_interface::BootInterfaceTarget;
use mac_address::MacAddress;
use model::bmc_suppression::BmcSuppressionSubsystem;
use model::expected_entity::ExpectedEntity;
use model::machine::MachineInterfaceSnapshot;
use model::site_explorer::{EndpointExplorationError, EndpointExplorationReport, ExploredEndpoint};
use sqlx::PgPool;

use crate::endpoint_lock::{EndpointExplorationGuard, EndpointExplorationLocks};
use crate::{EndpointExplorer, enrich_endpoint_exploration_report};

#[derive(Debug, thiserror::Error)]
pub enum EndpointExplorationServiceError {
    #[error(transparent)]
    Database(#[from] db::DatabaseError),

    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("endpoint exploration already in progress for {0}")]
    AlreadyInProgress(IpAddr),

    #[error("endpoint exploration is suppressed for BMC {bmc_mac_address} at {bmc_ip}")]
    Suppressed {
        bmc_ip: IpAddr,
        bmc_mac_address: MacAddress,
    },

    #[error(
        "an object of type {kind} was intended to be modified but did not have the expected version {version}"
    )]
    ConcurrentModification { kind: &'static str, version: String },

    #[error("endpoint refresh background task failed for {bmc_ip}: {message}")]
    BackgroundTaskFailed { bmc_ip: IpAddr, message: String },
}

pub(crate) struct EndpointProbeResult {
    pub(crate) result: Result<EndpointExplorationReport, EndpointExplorationError>,
    pub(crate) redfish_explore_duration: Duration,
    _guard: EndpointExplorationGuard,
}

/// Coordinates endpoint report generation across periodic and explicit refresh callers.
///
/// The low-level [`EndpointExplorer`] remains responsible only for talking to the BMC. This
/// service owns persistence for explicit refreshes and the in-process lock that prevents multiple
/// callers from probing the same endpoint concurrently.
#[derive(Clone)]
pub struct EndpointExplorationService {
    database_connection: PgPool,
    endpoint_explorer: Arc<dyn EndpointExplorer>,
    firmware_config: Arc<FirmwareConfig>,
    locks: EndpointExplorationLocks,
}

impl EndpointExplorationService {
    pub fn new(
        database_connection: PgPool,
        endpoint_explorer: Arc<dyn EndpointExplorer>,
        firmware_config: Arc<FirmwareConfig>,
    ) -> Self {
        Self {
            database_connection,
            endpoint_explorer,
            firmware_config,
            locks: EndpointExplorationLocks::default(),
        }
    }

    pub(crate) fn endpoint_explorer(&self) -> Arc<dyn EndpointExplorer> {
        self.endpoint_explorer.clone()
    }

    pub(crate) async fn firmware_config_snapshot(
        &self,
    ) -> Result<FirmwareConfigSnapshot, db::DatabaseError> {
        let host_firmware_configs =
            db::host_firmware_config::list_configs(&self.database_connection).await?;

        Ok(self
            .firmware_config
            .create_snapshot_with_overrides(host_firmware_configs))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn try_explore_endpoint(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        expected: Option<&ExpectedEntity>,
        last_exploration_error: Option<&EndpointExplorationError>,
        boot_interface: Option<BootInterfaceTarget>,
    ) -> Option<EndpointProbeResult> {
        let guard = self.locks.try_claim(address.ip())?;

        let redfish_explore_started_at = Instant::now();
        let result = self
            .endpoint_explorer
            .explore_endpoint(
                address,
                interface,
                expected,
                last_exploration_error,
                boot_interface.as_ref(),
            )
            .await;
        let redfish_explore_duration = redfish_explore_started_at.elapsed();

        Some(EndpointProbeResult {
            result,
            redfish_explore_duration,
            _guard: guard,
        })
    }

    pub(crate) fn try_claim_endpoint(&self, bmc_ip: IpAddr) -> Option<EndpointExplorationGuard> {
        self.locks.try_claim(bmc_ip)
    }

    pub async fn refresh_endpoint_report(
        &self,
        bmc_ip: IpAddr,
    ) -> Result<ExploredEndpoint, EndpointExplorationServiceError> {
        let mut txn = db::Transaction::begin(&self.database_connection).await?;

        let existing_endpoint = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EndpointExplorationServiceError::NotFound {
                kind: "explored_endpoint",
                id: bmc_ip.to_string(),
            })?;

        let bmc_interface = db::machine_interface::find_by_ip(&mut txn, bmc_ip)
            .await?
            .ok_or_else(|| EndpointExplorationServiceError::NotFound {
                kind: "machine_interface",
                id: bmc_ip.to_string(),
            })?;

        if db::bmc_suppression::is_suppressed(
            txn.as_pgconn(),
            bmc_interface.mac_address,
            BmcSuppressionSubsystem::SiteExplorer,
        )
        .await?
        {
            return Err(EndpointExplorationServiceError::Suppressed {
                bmc_ip,
                bmc_mac_address: bmc_interface.mac_address,
            });
        }

        let expected_machine =
            db::expected_machine::find_by_bmc_mac_address(&mut txn, bmc_interface.mac_address)
                .await?;
        let expected = if let Some(expected_machine) = expected_machine {
            Some(ExpectedEntity::Machine(expected_machine))
        } else if let Some(expected_switch) =
            db::expected_switch::find_by_bmc_mac_address(txn.as_pgconn(), bmc_interface.mac_address)
                .await?
        {
            Some(ExpectedEntity::Switch(expected_switch))
        } else {
            db::expected_power_shelf::find_by_bmc_mac_address(
                txn.as_pgconn(),
                bmc_interface.mac_address,
            )
            .await?
            .map(ExpectedEntity::PowerShelf)
        };

        txn.commit().await?;

        let service = self.clone();
        let bmc_address = SocketAddr::new(bmc_ip, 443);
        // The generated report is based on this row. Use its version for the eventual CAS so a
        // concurrent report update during the probe cannot be overwritten.
        let baseline_version = existing_endpoint.report_version;
        let boot_interface = existing_endpoint.boot_interface_target().map(Into::into);
        let existing_report = existing_endpoint.report;

        let join_handle = tokio::spawn(async move {
            let probe = service
                .try_explore_endpoint(
                    bmc_address,
                    &bmc_interface,
                    expected.as_ref(),
                    existing_report.last_exploration_error.as_ref(),
                    boot_interface,
                )
                .await
                .ok_or(EndpointExplorationServiceError::AlreadyInProgress(bmc_ip))?;

            let mut report = match probe.result {
                Ok(mut report) => {
                    let firmware_config = service.firmware_config_snapshot().await?;
                    enrich_endpoint_exploration_report(&mut report, &firmware_config);
                    report
                }
                Err(error) => {
                    let mut report = existing_report;
                    report.last_exploration_error = Some(error);
                    report
                }
            };
            report.last_exploration_latency = Some(probe.redfish_explore_duration);

            let mut txn = db::Transaction::begin(&service.database_connection).await?;
            if !db::explored_endpoints::try_update(
                bmc_ip,
                baseline_version,
                &report,
                false,
                &mut txn,
            )
            .await?
            {
                return Err(EndpointExplorationServiceError::ConcurrentModification {
                    kind: "explored_endpoint",
                    version: baseline_version.to_string(),
                });
            }

            let endpoint = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| EndpointExplorationServiceError::NotFound {
                    kind: "explored_endpoint",
                    id: bmc_ip.to_string(),
                })?;

            txn.commit().await?;
            Ok(endpoint)
        });

        match join_handle.await {
            Ok(result) => result,
            Err(error) => Err(EndpointExplorationServiceError::BackgroundTaskFailed {
                bmc_ip,
                message: error.to_string(),
            }),
        }
    }
}
