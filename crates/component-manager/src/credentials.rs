// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Credential resolution for direct component operations and passive reads.

use std::time::Duration;

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use db::DbSessionGuard;
use mac_address::MacAddress;

use crate::component_common::RackComponent;
use crate::error::ComponentManagerError;
use crate::types::ComponentCredentials;

// Cap each expected-inventory statement at five seconds, preserving a shorter
// operator limit. This PostgreSQL cap applies after snapshot setup; it does not
// bound connection acquisition, setup or the total helper duration.
const EXPECTED_CREDENTIAL_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolve credentials using an explicit component override, then Vault, then
/// expected inventory. NVOS uses its own override and credential key; other
/// components use BMC credentials from their matching expected-inventory table.
///
/// A credential-store error is returned without falling back to expected
/// inventory. Missing expected credentials return [`ComponentManagerError::NotFound`].
/// This function only reads credentials and never writes bootstrap credentials.
///
/// Expected-inventory statements have a five-second server timeout, preserving a
/// shorter configured limit. Callers own any overall deadline, including pool
/// acquisition and setup. Cancellation discards an active database connection and
/// releases its local pool slot; server-side cleanup may finish later.
pub async fn resolve_credentials(
    pool: &sqlx::PgPool,
    credential_reader: &dyn CredentialReader,
    component: RackComponent,
    bmc_mac: MacAddress,
    overrides: Option<&ComponentCredentials>,
) -> Result<Credentials, ComponentManagerError> {
    let credentials = overrides.and_then(|overrides| match component {
        RackComponent::SwitchNvos => overrides.nvos.as_ref(),
        RackComponent::ComputeBmc | RackComponent::SwitchBmc | RackComponent::PowerShelfPmc => {
            overrides.bmc.as_ref()
        }
    });
    match credentials {
        Some(credentials) => {
            tracing::info!(
                rack_component = ?component,
                management_mac_address = %bmc_mac,
                "using client credential override for direct dispatch"
            );
            Ok(credentials.clone())
        }
        None => fetch_credentials(pool, credential_reader, component, bmc_mac).await,
    }
}

async fn fetch_credentials(
    pool: &sqlx::PgPool,
    credential_reader: &dyn CredentialReader,
    component: RackComponent,
    bmc_mac: MacAddress,
) -> Result<Credentials, ComponentManagerError> {
    match fetch_vault_credentials(component, credential_reader, bmc_mac).await? {
        Some(credentials) => Ok(credentials),
        None => {
            let credentials = fetch_expected_credentials(component, pool, bmc_mac).await?;
            tracing::info!(
                rack_component = ?component,
                management_mac_address = %bmc_mac,
                "using expected inventory credentials because Vault credentials are absent"
            );
            Ok(credentials)
        }
    }
}

async fn fetch_vault_credentials(
    component: RackComponent,
    credential_reader: &dyn CredentialReader,
    bmc_mac: MacAddress,
) -> Result<Option<Credentials>, ComponentManagerError> {
    let key = match component {
        RackComponent::SwitchNvos => CredentialKey::SwitchNvosAdmin {
            bmc_mac_address: bmc_mac,
        },
        RackComponent::ComputeBmc | RackComponent::SwitchBmc | RackComponent::PowerShelfPmc => {
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: bmc_mac,
                },
            }
        }
    };
    credential_reader.get_credentials(&key).await.map_err(|e| {
        ComponentManagerError::Internal(format!("failed to fetch credentials for {key:?}: {e}"))
    })
}

async fn fetch_expected_credentials(
    component: RackComponent,
    pool: &sqlx::PgPool,
    bmc_mac: MacAddress,
) -> Result<Credentials, ComponentManagerError> {
    let map_error = |e: db::DatabaseError| {
        ComponentManagerError::Internal(format!(
            "failed to fetch expected credentials for {component:?} at {bmc_mac}: {e}"
        ))
    };
    let mut connection = DbSessionGuard::acquire(pool).await.map_err(map_error)?;
    let mut txn = connection
        .begin_read_only_snapshot(EXPECTED_CREDENTIAL_STATEMENT_TIMEOUT)
        .await
        .map_err(map_error)?;
    let credentials = match component {
        RackComponent::ComputeBmc => {
            db::expected_machine::find_by_bmc_mac_address(txn.as_pgconn(), bmc_mac)
                .await
                .map(|row| {
                    row.map(|row| Credentials::new(row.data.bmc_username, row.data.bmc_password))
                })
        }
        RackComponent::SwitchBmc => {
            db::expected_switch::find_by_bmc_mac_address(txn.as_pgconn(), bmc_mac)
                .await
                .map(|row| row.map(|row| Credentials::new(row.bmc_username, row.bmc_password)))
        }
        RackComponent::SwitchNvos => {
            db::expected_switch::find_by_bmc_mac_address(txn.as_pgconn(), bmc_mac)
                .await
                .map(|row| {
                    row.and_then(|row| {
                        row.nvos_username
                            .zip(row.nvos_password)
                            .map(|(username, password)| Credentials::new(username, password))
                    })
                })
        }
        RackComponent::PowerShelfPmc => {
            db::expected_power_shelf::find_by_bmc_mac_address(txn.as_pgconn(), bmc_mac)
                .await
                .map(|row| row.map(|row| Credentials::new(row.bmc_username, row.bmc_password)))
        }
    };
    let credentials = credentials.map_err(map_error)?;
    txn.commit().await.map_err(map_error)?;
    connection.return_to_pool();
    // An absent row is a successful read and must also return its healthy session.
    credentials.ok_or_else(|| {
        ComponentManagerError::NotFound(format!(
            "no credentials found for {component:?} at {bmc_mac}"
        ))
    })
}
