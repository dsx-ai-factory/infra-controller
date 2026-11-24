// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Switch endpoint resolution from expected inventory before ingestion.

use std::collections::HashMap;

use carbide_secrets::credentials::CredentialReader;
use carbide_utils::none_if_empty::NoneIfEmpty;
use mac_address::MacAddress;
use sqlx::PgPool;

use crate::component_common::RackComponent;
use crate::credentials::resolve_credentials;
use crate::error::ComponentManagerError;
use crate::nv_switch_manager::SwitchEndpoint;
use crate::types::ComponentCredentials;

/// Endpoint resolution outcome for one requested switch BMC MAC.
pub struct SwitchEndpointResult {
    /// BMC MAC identifying the requested switch, including unresolved switches.
    pub bmc_mac: MacAddress,
    /// Resolved endpoint or the inventory/credential error that prevented resolution.
    pub endpoint: Result<SwitchEndpoint, ComponentManagerError>,
}

/// Resolve switch endpoints without requiring managed switch records.
///
/// Addresses and hostnames come from discovered or preallocated interfaces using
/// the expected switch's primary NVOS port. Credentials use request overrides,
/// then Vault, then expected inventory; this function never writes credentials.
/// Returns one outcome per input MAC, in input order. Missing inventory, missing
/// NVOS addresses, and credential failures affect only the corresponding outcome;
/// a failure of the shared endpoint query fails the whole call.
pub async fn resolve_pre_ingestion_switch_endpoints(
    pool: &PgPool,
    credential_reader: &dyn CredentialReader,
    bmc_macs: &[MacAddress],
    overrides: Option<&ComponentCredentials>,
) -> Result<Vec<SwitchEndpointResult>, ComponentManagerError> {
    let rows = db::switch::find_switch_endpoints_by_bmc_macs(pool, bmc_macs)
        .await
        .map_err(|error| {
            ComponentManagerError::Internal(format!("db error resolving switch endpoints: {error}"))
        })?;
    let mut by_mac: HashMap<_, _> = rows.into_iter().map(|row| (row.bmc_mac, row)).collect();
    let mut results = Vec::with_capacity(bmc_macs.len());
    for &bmc_mac in bmc_macs {
        let endpoint = async {
            let row = by_mac.remove(&bmc_mac).ok_or_else(|| {
                ComponentManagerError::NotFound(format!(
                    "switch BMC MAC {bmc_mac} not found in expected inventory",
                ))
            })?;
            let (Some(nvos_mac), Some(nvos_ip)) = (row.nvos_mac, row.nvos_ip) else {
                return Err(ComponentManagerError::NotFound(format!(
                    "NVOS MAC or IP not available for BMC {bmc_mac}",
                )));
            };
            let bmc_credentials = resolve_credentials(
                pool,
                credential_reader,
                RackComponent::SwitchBmc,
                bmc_mac,
                overrides,
            )
            .await?;
            let nvos_credentials = resolve_credentials(
                pool,
                credential_reader,
                RackComponent::SwitchNvos,
                bmc_mac,
                overrides,
            )
            .await?;

            Ok(SwitchEndpoint {
                bmc_ip: row.bmc_ip,
                bmc_mac,
                nvos_ip,
                nvos_mac,
                bmc_credentials,
                nvos_credentials,
                nvos_host_name: row.nvos_hostname.none_if_empty(),
            })
        }
        .await;
        results.push(SwitchEndpointResult { bmc_mac, endpoint });
    }
    Ok(results)
}

#[cfg(test)]
mod tests;
