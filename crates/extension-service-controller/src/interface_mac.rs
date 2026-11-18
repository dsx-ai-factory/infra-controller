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

//! Durable MAC allocation for extension-service interfaces.

use carbide_uuid::extension_service::ExtensionServiceId;
use db::extension_service::InsertExtensionServiceInterfaceMacError;
use db::{DatabaseError, DatabaseResult};
use model::extension_service::ExtensionServiceInterfaceMac;
use sqlx::PgConnection;

const MAX_MAC_ALLOCATION_ATTEMPTS: u32 = 64;

/// Returns the durable MAC for one service interface, allocating it when absent.
/// Bounded deterministic candidates make collision recovery restart-safe without changing an
/// existing identity.
pub async fn ensure_interface_mac(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    interface_ordinal: u32,
) -> DatabaseResult<ExtensionServiceInterfaceMac> {
    // Reuse identity before attempting an insert so ordinary reconciliation performs no write.
    if let Some(existing) = find_interface_mac(txn, service_id, interface_ordinal).await? {
        return Ok(existing);
    }

    // Each insert owns a savepoint in api-db, so an expected uniqueness conflict leaves this
    // transaction usable.
    for attempt in 0..MAX_MAC_ALLOCATION_ATTEMPTS {
        let candidate =
            ExtensionServiceInterfaceMac::candidate(service_id, interface_ordinal, attempt);
        match db::extension_service::insert_interface_mac(txn, candidate).await {
            Ok(()) => return Ok(candidate),
            Err(InsertExtensionServiceInterfaceMacError::MacAddressCollision { .. }) => continue,
            Err(InsertExtensionServiceInterfaceMacError::ExistingAssignment { .. }) => {
                // Return the committed assignment after a concurrent insert wins the primary-key
                // race, treating the database row as authoritative.
                return find_interface_mac(txn, service_id, interface_ordinal)
                    .await?
                    .ok_or_else(|| DatabaseError::Internal {
                        message: format!(
                            "extension service {service_id} reported an existing MAC assignment for interface {interface_ordinal}, but it could not be read"
                        ),
                    });
            }
            Err(InsertExtensionServiceInterfaceMacError::ServiceNotFound { .. }) => {
                return Err(DatabaseError::NotFoundError {
                    kind: "extension_service",
                    id: service_id.to_string(),
                });
            }
            Err(InsertExtensionServiceInterfaceMacError::InvalidInterfaceOrdinal(ordinal)) => {
                return Err(DatabaseError::InvalidArgument(format!(
                    "extension-service interface ordinal {ordinal} exceeds the database representation"
                )));
            }
            Err(InsertExtensionServiceInterfaceMacError::Database(error)) => return Err(error),
        }
    }

    Err(DatabaseError::ResourceExhausted(format!(
        "could not allocate a unique MAC for extension service {service_id} interface {interface_ordinal} after {MAX_MAC_ALLOCATION_ATTEMPTS} attempts"
    )))
}

/// Finds one ordinal-specific assignment from the service's small ordered identity set.
/// Centralizing the lookup keeps concurrent insert recovery and the read-before-write path
/// identical.
async fn find_interface_mac(
    txn: &mut PgConnection,
    service_id: ExtensionServiceId,
    interface_ordinal: u32,
) -> DatabaseResult<Option<ExtensionServiceInterfaceMac>> {
    Ok(db::extension_service::find_interface_macs(txn, service_id)
        .await?
        .into_iter()
        .find(|assignment| assignment.interface_ordinal == interface_ordinal))
}
