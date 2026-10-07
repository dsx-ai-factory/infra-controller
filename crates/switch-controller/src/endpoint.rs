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

//! Helpers for building `SwitchEndpoint` values from api-db endpoint rows.

use std::sync::Arc;

use carbide_secrets::credentials::{CredentialKey, CredentialManager, Credentials};
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::switch::SwitchId;
use component_manager::nv_switch_manager::SwitchEndpoint;
use db::switch::SwitchEndpointRow;
use mac_address::MacAddress;
use state_controller::state_handler::StateHandlerError;

async fn fetch_switch_nvos_credentials(
    credential_manager: &Arc<dyn CredentialManager>,
    bmc_mac: MacAddress,
) -> Result<Credentials, StateHandlerError> {
    let key = CredentialKey::SwitchNvosAdmin {
        bmc_mac_address: bmc_mac,
    };
    credential_manager
        .get_credentials(&key)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to read NVOS admin credentials for BMC MAC {bmc_mac}: {error}"
            ))
        })?
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre::eyre!(
                "no NVOS admin credentials configured for BMC MAC {bmc_mac}"
            ))
        })
}

pub fn switch_endpoint_from_row(
    row: &SwitchEndpointRow,
    nvos_credentials: Credentials,
) -> Result<SwitchEndpoint, StateHandlerError> {
    let (Some(nvos_mac), Some(nvos_ip)) = (row.nvos_mac, row.nvos_ip) else {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "switch {:?}: missing NVOS MAC or IP required for component manager operations",
            row.switch_id
        )));
    };

    Ok(SwitchEndpoint {
        bmc_ip: row.bmc_ip,
        bmc_mac: row.bmc_mac,
        nvos_ip,
        nvos_mac,
        bmc_credentials: nvos_credentials.clone(),
        nvos_credentials,
        nvos_host_name: row.nvos_hostname.clone().none_if_empty(),
    })
}

/// Resolve a switch to a CM `SwitchEndpoint` using the shared api-db query also
/// used by the component-manager gRPC handler.
pub async fn resolve_switch_endpoint(
    switch_id: &SwitchId,
    db_pool: &sqlx::PgPool,
    credential_manager: &Arc<dyn CredentialManager>,
) -> Result<SwitchEndpoint, StateHandlerError> {
    let rows = db::switch::find_switch_endpoints_by_ids(db_pool, std::slice::from_ref(switch_id))
        .await
        .map_err(StateHandlerError::from)?;
    let row = rows.into_iter().next().ok_or_else(|| {
        StateHandlerError::GenericError(eyre::eyre!(
            "switch {:?}: no endpoint row found in database",
            switch_id
        ))
    })?;
    let nvos_credentials = fetch_switch_nvos_credentials(credential_manager, row.bmc_mac).await?;
    switch_endpoint_from_row(&row, nvos_credentials)
}
