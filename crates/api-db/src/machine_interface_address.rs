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
use std::net::IpAddr;

use carbide_network::ip::{IdentifyAddressFamily, IpAddressFamily};
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::switch::SwitchId;
use mac_address::MacAddress;
use model::allocation_type::{AllocationType, AssignStaticResult};
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;
use sqlx::{FromRow, PgConnection};

use super::DatabaseError;
use crate::db_read::DbReader;

#[cfg(test)]
mod test_find_by_address;

/// Returned when an address is already held by an interface.
#[derive(thiserror::Error, Debug)]
#[error("address already in use: {0} by {1} in network segment {2} (interface: {3})")]
pub struct AddressAlreadyInUseError(
    pub IpAddr,
    pub MacAddress,
    pub NetworkSegmentId,
    pub MachineInterfaceId,
);

#[derive(Debug, FromRow, Clone)]
pub struct MachineInterfaceAddress {
    pub address: IpAddr,
}

#[derive(Debug, FromRow, Clone)]
pub struct MachineInterfaceAddressWithType {
    pub address: IpAddr,
    pub allocation_type: AllocationType,
}

pub async fn find_ipv4_for_interface(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
) -> Result<MachineInterfaceAddress, DatabaseError> {
    let query =
        "SELECT * FROM machine_interface_addresses WHERE interface_id = $1 AND family(address) = 4";
    sqlx::query_as(query)
        .bind(interface_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Looks up which machine interface owns an IP, with association, segment, role, and allocation
/// metadata.
///
/// The IP finder uses `interface_type` together with `allocation_type` and segment metadata to
/// distinguish static BMC addresses from static Data addresses.
pub async fn find_by_address(
    txn: impl DbReader<'_>,
    address: IpAddr,
) -> Result<Option<MachineInterfaceSearchResult>, DatabaseError> {
    let query = "SELECT mi.id, mi.machine_id, mi.switch_id, mi.interface_type,
                mi.mac_address, mi.segment_id,
                ns.name, ns.network_segment_type, mia.allocation_type
            FROM machine_interface_addresses mia
            INNER JOIN machine_interfaces mi ON mi.id = mia.interface_id
            INNER JOIN network_segments ns ON ns.id = mi.segment_id
            WHERE mia.address = $1::inet
        ";
    sqlx::query_as(query)
        .bind(address)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// `delete` removes all addresses during interface teardown. The caller must
/// delete the interface in the same transaction; use a scoped deletion helper
/// when the interface remains so its allocation removal is recorded.
pub async fn delete(
    txn: &mut PgConnection,
    interface_id: &MachineInterfaceId,
) -> Result<(), DatabaseError> {
    lock_interface_for_address_change(txn, *interface_id).await?;
    let query = "DELETE FROM machine_interface_addresses WHERE interface_id = $1";
    sqlx::query(query)
        .bind(interface_id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

// Lock the owner before changing its addresses. An allocator holds that same
// lock while checking removal history, so it cannot mistake expiry for first
// allocation. A missing interface keeps the deletion's existing no-op behavior.
async fn lock_interface_for_address_change(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
) -> Result<(), DatabaseError> {
    let query = "SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE";
    sqlx::query_scalar::<_, MachineInterfaceId>(query)
        .bind(interface_id)
        .fetch_optional(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Find all addresses for an interface, including their allocation type.
pub async fn find_for_interface(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
) -> Result<Vec<MachineInterfaceAddressWithType>, DatabaseError> {
    let query =
        "SELECT address, allocation_type FROM machine_interface_addresses WHERE interface_id = $1";
    sqlx::query_as(query)
        .bind(interface_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Find the allocation type of the existing address for a given
/// interface and address family, if one exists.
pub async fn find_allocation_type_for_family(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    family: IpAddressFamily,
) -> Result<Option<AllocationType>, DatabaseError> {
    let query = "SELECT allocation_type FROM machine_interface_addresses WHERE interface_id = $1 AND family(address) = $2";
    let result: Option<(AllocationType,)> = sqlx::query_as(query)
        .bind(interface_id)
        .bind(family.pg_family())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(result.map(|(t,)| t))
}

/// Delete the address for a given interface, address family, and
/// allocation type. Returns true if a row was deleted.
pub async fn delete_by_interface_family(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    family: IpAddressFamily,
    allocation_type: AllocationType,
) -> Result<bool, DatabaseError> {
    lock_interface_for_address_change(txn, interface_id).await?;
    let query = "DELETE FROM machine_interface_addresses WHERE interface_id = $1 AND family(address) = $2 AND allocation_type = $3";
    let removed = sqlx::query(query)
        .bind(interface_id)
        .bind(family.pg_family())
        .bind(allocation_type)
        .execute(&mut *txn)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(|e| DatabaseError::query(query, e))?;
    if removed && allocation_type != AllocationType::Slaac {
        crate::machine_interface::record_allocation_removal(txn, interface_id, family).await?;
    }
    Ok(removed)
}

/// Delete a specific address from a specific interface. Returns true if a
/// matching row was deleted. Scoping by `interface_id` ensures an operator
/// remove-address call only removes the caller's own address, never another
/// interface's row that happens to hold the same IP.
pub async fn delete_by_interface_and_address(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    address: IpAddr,
    allocation_type: AllocationType,
) -> Result<bool, DatabaseError> {
    lock_interface_for_address_change(txn, interface_id).await?;
    let query = "DELETE FROM machine_interface_addresses WHERE interface_id = $1 AND address = $2::inet AND allocation_type = $3";
    let removed = sqlx::query(query)
        .bind(interface_id)
        .bind(address)
        .bind(allocation_type)
        .execute(&mut *txn)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(|e| DatabaseError::query(query, e))?;
    if removed && allocation_type != AllocationType::Slaac {
        crate::machine_interface::record_allocation_removal(
            txn,
            interface_id,
            address.address_family(),
        )
        .await?;
    }
    Ok(removed)
}

/// `insert` assigns an unowned address to an interface.
///
/// `ON CONFLICT DO NOTHING` leaves the caller's transaction usable so we can
/// identify the current owner. Repeating the same assignment for the same
/// interface is idempotent; a different owner or allocation type returns
/// [`AddressAlreadyInUseError`]. An interface that already has another address
/// in the same family returns [`DatabaseError::FailedPrecondition`]. Allocation
/// callers decide whether to choose another address; this function only
/// attempts the requested address once.
pub async fn insert(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    address: IpAddr,
    allocation_type: AllocationType,
) -> Result<(), DatabaseError> {
    let query = "INSERT INTO machine_interface_addresses (interface_id, address, allocation_type)
        VALUES ($1::uuid, $2::inet, $3)
        ON CONFLICT DO NOTHING
        RETURNING interface_id";

    let address_inserted = sqlx::query_scalar::<_, MachineInterfaceId>(query)
        .bind(interface_id)
        .bind(address)
        .bind(allocation_type)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .is_some();
    if address_inserted {
        return Ok(());
    }

    let Some(address_owner) = find_by_address(&mut *txn, address).await? else {
        if let Some(existing_in_family) = find_for_interface(&mut *txn, interface_id)
            .await?
            .into_iter()
            .find(|candidate| {
                candidate
                    .address
                    .is_address_family(address.address_family())
            })
        {
            return Err(DatabaseError::FailedPrecondition(format!(
                "interface {interface_id} already has address {} in the same family as {address}",
                existing_in_family.address,
            )));
        }
        return Err(DatabaseError::internal(format!(
            "address {address} could not be assigned to interface {interface_id}, and no conflicting assignment was found"
        )));
    };
    if address_owner.id == interface_id && address_owner.allocation_type == allocation_type {
        return Ok(());
    }
    Err(AddressAlreadyInUseError(
        address,
        address_owner.mac_address,
        address_owner.segment_id,
        address_owner.id,
    )
    .into())
}

/// Assign a static address to an interface. If the interface already
/// has an address for the same family, the behavior depends on its
/// allocation type:
///
/// - `Static`: the old static address is replaced.
/// - `Dhcp` or `Slaac`: the managed allocation is removed and
///   replaced with the static assignment.
pub async fn assign_static(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    address: IpAddr,
) -> Result<AssignStaticResult, DatabaseError> {
    lock_interface_for_address_change(txn, interface_id).await?;
    let family = address.address_family();

    let existing = find_allocation_type_for_family(&mut *txn, interface_id, family).await?;

    let result = match existing {
        Some(allocation_type @ (AllocationType::Dhcp | AllocationType::Slaac)) => {
            delete_by_interface_family(&mut *txn, interface_id, family, allocation_type).await?;
            AssignStaticResult::ReplacedDhcp
        }
        Some(AllocationType::Static) => {
            delete_by_interface_family(&mut *txn, interface_id, family, AllocationType::Static)
                .await?;
            AssignStaticResult::ReplacedStatic
        }
        None => AssignStaticResult::Assigned,
    };

    insert(txn, interface_id, address, AllocationType::Static).await?;

    Ok(result)
}

/// Delete an address allocation of the given type. Returns at most one owning
/// interface in a vector so callers can share the hostname-resync path with
/// MAC-scoped deletion.
pub async fn delete_by_address(
    txn: &mut PgConnection,
    address: IpAddr,
    allocation_type: AllocationType,
) -> Result<Vec<MachineInterfaceId>, DatabaseError> {
    let Some(owner) = find_by_address(&mut *txn, address).await? else {
        return Ok(Vec::new());
    };
    let removed = delete_by_interface_and_address(txn, owner.id, address, allocation_type).await?;
    Ok(removed.then_some(owner.id).into_iter().collect())
}

/// Delete an address allocation for a given (ip, mac) pair, which
/// of course only actually deletes when the pair matches.
///
/// Returns the interfaces that owned the deleted allocations (normally one,
/// empty if the pair matched nothing) so callers can resync each one's hostname
/// against the authoritative deleted rows rather than a separate lookup.
pub async fn delete_by_address_and_mac(
    txn: &mut PgConnection,
    address: IpAddr,
    mac_address: mac_address::MacAddress,
    allocation_type: AllocationType,
) -> Result<Vec<MachineInterfaceId>, DatabaseError> {
    let Some(owner) = find_by_address(&mut *txn, address)
        .await?
        .filter(|owner| owner.mac_address == mac_address)
    else {
        return Ok(Vec::new());
    };
    let removed = delete_by_interface_and_address(txn, owner.id, address, allocation_type).await?;
    Ok(removed.then_some(owner.id).into_iter().collect())
}

/// Check whether an interface has any address assigned for the
/// given address family.
///
/// This is used by the DHCPDISCOVER flow to decide whether to
/// re-allocate after a lease expiration. If the interface still
/// has an address for the family (static or DHCP), no re-allocation
// is needed.
pub async fn has_address_for_family(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    family: IpAddressFamily,
) -> Result<bool, DatabaseError> {
    let query = "SELECT EXISTS(SELECT 1 FROM machine_interface_addresses WHERE interface_id = $1 AND family(address) = $2)";
    sqlx::query_scalar(query)
        .bind(interface_id)
        .bind(family.pg_family())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Row shape for [`find_by_address`]: interface identity, association, role, owning segment, and
/// how the address was assigned (DHCP vs static / operator-configured).
#[derive(Debug, FromRow)]
pub struct MachineInterfaceSearchResult {
    pub id: MachineInterfaceId,
    pub machine_id: Option<MachineId>,
    pub switch_id: Option<SwitchId>,
    pub interface_type: InterfaceType,
    pub mac_address: MacAddress,
    pub segment_id: NetworkSegmentId,
    pub name: String,
    pub network_segment_type: NetworkSegmentType,
    pub allocation_type: AllocationType,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Creates an addressless interface for address-ownership tests.
    async fn create_test_interface(
        txn: &mut PgConnection,
        segment_id: NetworkSegmentId,
        mac_address: MacAddress,
        hostname: &str,
    ) -> Result<MachineInterfaceId, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                (segment_id, mac_address, primary_interface, hostname)
             VALUES ($1, $2, false, $3)
             RETURNING id",
        )
        .bind(segment_id)
        .bind(mac_address)
        .bind(hostname)
        .fetch_one(txn)
        .await
    }

    /// Races one address insert in its own transaction.
    async fn insert_after_barrier(
        pool: &sqlx::PgPool,
        barrier: Arc<tokio::sync::Barrier>,
        interface_id: MachineInterfaceId,
        address: IpAddr,
        allocation_type: AllocationType,
    ) -> Result<(), DatabaseError> {
        barrier.wait().await;
        let mut txn = crate::Transaction::begin(pool).await?;

        match insert(txn.as_pgconn(), interface_id, address, allocation_type).await {
            Ok(()) => txn.commit().await,
            Err(error) => {
                txn.rollback().await?;
                Err(error)
            }
        }
    }

    /// Verifies the new SLAAC allocation type survives a database round trip.
    #[crate::sqlx_test]
    async fn slaac_allocation_type_round_trips(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        // Create the minimal segment and interface rows needed to own an address.
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version) VALUES ($1, 'V1-T0') RETURNING id",
        )
        .bind("slaac-roundtrip")
        .fetch_one(txn.as_mut())
        .await?;
        let interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
             VALUES ($1, $2::macaddr, true, 'slaac-roundtrip') RETURNING id",
        )
        .bind(segment_id)
        .bind("02:00:00:00:00:01")
        .fetch_one(txn.as_mut())
        .await?;

        // Insert a SLAAC allocation through the public helper and read it back.
        insert(
            txn.as_mut(),
            interface_id,
            "2001:db8::10".parse()?,
            AllocationType::Slaac,
        )
        .await?;
        let addresses = find_for_interface(txn.as_mut(), interface_id).await?;

        // Verify the persisted row preserved the new allocation type.
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);

        txn.rollback().await?;
        Ok(())
    }

    /// Verifies that concurrent inserts leave one owner and conflicts stay actionable.
    #[crate::sqlx_test]
    async fn concurrent_inserts_keep_one_address_owner(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut setup_txn = pool.begin().await?;
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('address-owner-race', 'V1-T0')
             RETURNING id",
        )
        .fetch_one(&mut *setup_txn)
        .await?;
        let first_mac: MacAddress = "02:00:00:00:00:11".parse()?;
        let second_mac: MacAddress = "02:00:00:00:00:12".parse()?;
        let first_interface_id = create_test_interface(
            &mut setup_txn,
            segment_id,
            first_mac,
            "address-owner-race-first",
        )
        .await?;
        let second_interface_id = create_test_interface(
            &mut setup_txn,
            segment_id,
            second_mac,
            "address-owner-race-second",
        )
        .await?;
        setup_txn.commit().await?;

        let address: IpAddr = "192.0.2.10".parse()?;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (first_result, second_result) = tokio::join!(
            insert_after_barrier(
                &pool,
                barrier.clone(),
                first_interface_id,
                address,
                AllocationType::Static,
            ),
            insert_after_barrier(
                &pool,
                barrier,
                second_interface_id,
                address,
                AllocationType::Static,
            ),
        );

        let (owner_id, owner_mac, conflict) = match (first_result, second_result) {
            (Ok(()), Err(error)) => (first_interface_id, first_mac, error),
            (Err(error), Ok(())) => (second_interface_id, second_mac, error),
            results => panic!("expected one address owner and one conflict, got {results:?}"),
        };
        match conflict {
            DatabaseError::AddressAlreadyInUse(AddressAlreadyInUseError(
                conflict_address,
                conflict_mac,
                conflict_segment_id,
                conflict_interface_id,
            )) => {
                assert_eq!(conflict_address, address);
                assert_eq!(conflict_mac, owner_mac);
                assert_eq!(conflict_segment_id, segment_id);
                assert_eq!(conflict_interface_id, owner_id);
            }
            error => panic!("expected an address-in-use error, got {error:?}"),
        }

        let owner_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM machine_interface_addresses WHERE address = $1::inet",
        )
        .bind(address)
        .fetch_one(&pool)
        .await?;
        assert_eq!(owner_count, 1);

        // Concurrent observations of the same SLAAC address for one interface
        // are replays of the same ownership claim, so both callers succeed.
        let slaac_address: IpAddr = "2001:db8::10".parse()?;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (first_result, second_result) = tokio::join!(
            insert_after_barrier(
                &pool,
                barrier.clone(),
                first_interface_id,
                slaac_address,
                AllocationType::Slaac,
            ),
            insert_after_barrier(
                &pool,
                barrier,
                first_interface_id,
                slaac_address,
                AllocationType::Slaac,
            ),
        );
        first_result?;
        second_result?;

        let slaac_owner_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM machine_interface_addresses WHERE address = $1::inet",
        )
        .bind(slaac_address)
        .fetch_one(&pool)
        .await?;
        assert_eq!(slaac_owner_count, 1);

        Ok(())
    }

    /// Verifies that a second address in the same family is rejected.
    #[crate::sqlx_test]
    async fn insert_rejects_another_address_in_the_same_family(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('address-family-conflict', 'V1-T0')
             RETURNING id",
        )
        .fetch_one(&mut *txn)
        .await?;
        let interface_id = create_test_interface(
            &mut txn,
            segment_id,
            "02:00:00:00:00:14".parse()?,
            "address-family-conflict",
        )
        .await?;
        let existing_address: IpAddr = "2001:db8::10".parse()?;
        insert(
            &mut txn,
            interface_id,
            existing_address,
            AllocationType::Slaac,
        )
        .await?;

        let requested_address: IpAddr = "2001:db8::11".parse()?;
        let error = insert(
            &mut txn,
            interface_id,
            requested_address,
            AllocationType::Slaac,
        )
        .await
        .expect_err("a second IPv6 address on the same interface should be rejected");
        match error {
            DatabaseError::FailedPrecondition(message) => {
                assert!(
                    message.contains(&format!("already has address {existing_address}")),
                    "{message}"
                );
                assert!(
                    message.contains(&requested_address.to_string()),
                    "{message}"
                );
            }
            error => panic!("expected a failed-precondition error, got {error:?}"),
        }

        txn.rollback().await?;
        Ok(())
    }

    /// Verifies that masked network values cannot bypass address ownership.
    #[crate::sqlx_test]
    async fn machine_interface_addresses_require_host_form(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('host-addresses-only', 'V1-T0')
             RETURNING id",
        )
        .fetch_one(&mut *txn)
        .await?;
        let interface_id = create_test_interface(
            &mut txn,
            segment_id,
            "02:00:00:00:00:13".parse()?,
            "host-addresses-only",
        )
        .await?;

        let error = sqlx::query(
            "INSERT INTO machine_interface_addresses (interface_id, address)
             VALUES ($1, '192.0.2.20/24'::inet)",
        )
        .bind(interface_id)
        .execute(&mut *txn)
        .await
        .expect_err("a machine-interface address with a network mask should be rejected");
        match error {
            sqlx::Error::Database(error) => assert_eq!(
                error.constraint(),
                Some("machine_interface_addresses_host_address_check"),
            ),
            error => panic!("expected a check-constraint error, got {error:?}"),
        }

        txn.rollback().await?;
        Ok(())
    }
}
