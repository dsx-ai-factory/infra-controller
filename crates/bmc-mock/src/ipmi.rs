/*
 * SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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

//! IPMI-over-HTTP mock handler for testing.
//!
//! Receives JSON requests from `IPMIToolHttpImpl` and translates them
//! into `BmcCommand::SetSystemPower` calls to machine-a-tron.

use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::SystemPowerControl;
use crate::bmc_state::BmcState;
use crate::redfish::log_service::LogEntryDraft;

/// Request body for IPMI mock endpoint.
#[derive(Debug, Deserialize)]
struct IpmiRequest {
    action: String,
}

/// Response body for IPMI mock endpoint.
#[derive(Debug, Serialize)]
struct IpmiResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl IpmiResponse {
    fn ok() -> Self {
        Self {
            success: true,
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(msg.into()),
        }
    }
}

/// Add IPMI routes to the router.
pub(super) fn add_routes(router: Router<BmcState>) -> Router<BmcState> {
    router.route("/ipmi", post(handle_ipmi))
}

async fn handle_ipmi(
    axum::extract::State(state): axum::extract::State<BmcState>,
    Json(req): Json<IpmiRequest>,
) -> Json<IpmiResponse> {
    tracing::debug!(action = %req.action, "IPMI mock request");

    let Some(ref callbacks) = state.callbacks else {
        tracing::error!("IPMI request received but IPMI handler is not configured");
        return Json(IpmiResponse::err("IPMI handler is not configured"));
    };

    let response = match req.action.as_str() {
        "chassis_power_reset" => {
            tracing::info!("IPMI: chassis power reset");
            match callbacks.send_power_command(SystemPowerControl::ForceRestart) {
                Ok(()) => {
                    if let Some(system) = state.system_state.primary_system_odata_id() {
                        state.record_event(LogEntryDraft::reset_requested(
                            &system,
                            SystemPowerControl::ForceRestart,
                        ));
                    }
                    IpmiResponse::ok()
                }
                Err(e) => {
                    tracing::error!(error = ?e, "chassis power reset failed");
                    IpmiResponse::err(format!("power command failed: {:?}", e))
                }
            }
        }
        "bmc_cold_reset" => {
            if let Some(manager) = state.manager.primary_odata_id() {
                state.record_log(LogEntryDraft::manager_resetting(&manager, "IPMI"));
            }
            let offline_for = state.reset();
            tracing::info!(?offline_for, "IPMI: BMC cold reset");
            IpmiResponse::ok()
        }
        "dpu_legacy_boot" => {
            tracing::info!("IPMI: dpu legacy boot");
            match callbacks.send_power_command(SystemPowerControl::ForceRestart) {
                Ok(()) => IpmiResponse::ok(),
                Err(e) => {
                    tracing::error!(error = ?e, "dpu legacy boot failed");
                    IpmiResponse::err(format!("power command failed: {:?}", e))
                }
            }
        }
        other => {
            tracing::warn!(action = %other, "unknown IPMI action");
            IpmiResponse::err(format!("unknown action: {}", other))
        }
    };

    Json(response)
}
