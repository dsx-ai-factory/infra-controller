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
//! Provisioning suppression shared by periodic scans and manual report refreshes.

use std::net::IpAddr;

use carbide_instrument::{Event, LabelValue};
use mac_address::MacAddress;
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem};
use sqlx::PgConnection;

use crate::EndpointExplorationServiceError;

/// The exploration entry point that observed provisioning suppression.
#[derive(Clone, Copy, Debug, LabelValue)]
pub enum ExplorationRequest {
    /// An endpoint considered for a periodic scan.
    Periodic,
    /// A request to prioritize exploration in the next scan.
    ReExplore,
    /// An immediate report refresh.
    Refresh,
}

#[derive(Event)]
#[event(
    event_name = "site_explorer_dpf_exploration_deferred",
    metric_name = "carbide_site_explorer_dpf_explorations_deferred_total",
    component = "site-explorer",
    log = info,
    metric = counter,
    message = "Exploration paused during DPU provisioning; resumes automatically when all Site Explorer suppressions are cleared",
    describe = "Number of periodic endpoint candidates skipped, re-exploration requests rejected, or refresh requests deferred during DPF provisioning, by request type.",
)]
struct DpfExplorationDeferred {
    #[label]
    request: ExplorationRequest,
    #[context]
    bmc_ip: IpAddr,
}

pub(crate) fn record_deferred(bmc_ip: IpAddr, request: ExplorationRequest) {
    carbide_instrument::emit(DpfExplorationDeferred { request, bmc_ip });
}

/// Checks suppression before a manual exploration. DPF suppression records a
/// deferred-request metric; refresh callers queue the request instead of rejecting it.
pub async fn check_exploration_allowed(
    conn: &mut PgConnection,
    bmc_ip: IpAddr,
    bmc_mac_address: MacAddress,
    request: ExplorationRequest,
) -> Result<(), EndpointExplorationServiceError> {
    let sources = db::bmc_suppression::find_for_bmc(
        conn,
        bmc_mac_address,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await?;
    if sources.contains(&BmcSuppressionSource::DpfProvisioning) {
        record_deferred(bmc_ip, request);
        return Err(EndpointExplorationServiceError::DpfProvisioning {
            bmc_ip,
            bmc_mac_address,
        });
    }
    if !sources.is_empty() {
        return Err(EndpointExplorationServiceError::Suppressed {
            bmc_ip,
            bmc_mac_address,
        });
    }
    Ok(())
}
