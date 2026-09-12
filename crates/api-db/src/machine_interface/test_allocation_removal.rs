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

use std::time::Duration;

use carbide_network::ip::{IdentifyAddressFamily, IpAddressFamily};
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use sqlx::{PgConnection, PgPool};

use super::{can_apply_expected_allocation, find_optional_for_update_by_ip, lock_for_allocation};
use crate::machine_interface_address as addresses;

const REMOVAL_MIGRATION: &str =
    include_str!("../../migrations/20260911055455_machine_interface_allocation_removal.sql");

async fn create_interface(txn: &mut PgConnection) -> crate::DatabaseResult<MachineInterfaceId> {
    let query = "INSERT INTO network_segments (name, version)
        VALUES ('allocation-removal', 'V1-T0') RETURNING id";
    let segment_id: NetworkSegmentId = sqlx::query_scalar(query)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| crate::DatabaseError::query(query, error))?;
    let query =
        "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname)
        VALUES ($1, '02:00:00:00:00:01', false, 'allocation-removal') RETURNING id";
    sqlx::query_scalar(query)
        .bind(segment_id)
        .fetch_one(txn)
        .await
        .map_err(|error| crate::DatabaseError::query(query, error))
}

#[crate::sqlx_test]
async fn discovery_ip_lookup_locks_interface_before_address(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let interface_id = create_interface(&mut txn).await?;
    let address = "192.0.2.1".parse()?;
    addresses::insert(&mut txn, interface_id, address, AllocationType::Dhcp).await?;
    txn.commit().await?;

    let mut owner_txn = pool.begin().await?;
    lock_for_allocation(&mut owner_txn, interface_id).await?;
    let owner_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(owner_txn.as_mut())
        .await?;
    let mut lookup_txn = pool.begin().await?;
    let lookup_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(lookup_txn.as_mut())
        .await?;

    let lookup = async move {
        let found = find_optional_for_update_by_ip(&mut lookup_txn, address).await?;
        lookup_txn.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(found)
    };
    let observer_pool = &pool;
    let probe_address_lock = async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked_by_owner: bool =
                    sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                        .bind(owner_pid)
                        .bind(lookup_pid)
                        .fetch_one(observer_pool)
                        .await?;
                if blocked_by_owner {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("discovery lookup must wait for the interface lock")?;

        let mut probe_txn = observer_pool.begin().await?;
        let address_lock_result: Result<MachineInterfaceId, sqlx::Error> = sqlx::query_scalar(
            "SELECT interface_id FROM machine_interface_addresses
             WHERE interface_id = $1 AND address = $2::inet FOR UPDATE NOWAIT",
        )
        .bind(interface_id)
        .bind(address)
        .fetch_one(probe_txn.as_mut())
        .await;
        // Release both transactions before asserting so a failed probe
        // cannot leave discovery waiting for its interface.
        probe_txn.rollback().await?;
        owner_txn.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(address_lock_result)
    };
    let (lookup_result, probe_result) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(lookup, probe_address_lock)
    })
    .await
    .expect("discovery lookup and its lock probe must finish");

    let locked_owner =
        probe_result?.expect("discovery must not lock the address while waiting for its interface");
    assert_eq!(locked_owner, interface_id);
    let found = lookup_result?.expect("the discovery address must still resolve");
    assert_eq!(found.id, interface_id);
    assert_eq!(found.addresses, vec![address]);
    Ok(())
}

#[crate::sqlx_test]
async fn only_removed_stateful_families_are_recorded(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    enum Removal {
        Address,
        Mac,
        Family,
        InterfaceAddress,
        WrongMac,
    }
    struct Case {
        name: &'static str,
        address: &'static str,
        allocation_type: AllocationType,
        removal: Removal,
        recorded: bool,
    }
    let cases = [
        Case {
            name: "address expiry",
            address: "192.0.2.1",
            allocation_type: AllocationType::Dhcp,
            removal: Removal::Address,
            recorded: true,
        },
        Case {
            name: "MAC expiry",
            address: "2001:db8::1",
            allocation_type: AllocationType::Dhcp,
            removal: Removal::Mac,
            recorded: true,
        },
        Case {
            name: "family replacement",
            address: "192.0.2.1",
            allocation_type: AllocationType::Static,
            removal: Removal::Family,
            recorded: true,
        },
        Case {
            name: "operator removal",
            address: "2001:db8::1",
            allocation_type: AllocationType::Static,
            removal: Removal::InterfaceAddress,
            recorded: true,
        },
        Case {
            name: "SLAAC is not stateful",
            address: "2001:db8::1",
            allocation_type: AllocationType::Slaac,
            removal: Removal::Family,
            recorded: false,
        },
        Case {
            name: "wrong MAC is not a removal",
            address: "192.0.2.1",
            allocation_type: AllocationType::Dhcp,
            removal: Removal::WrongMac,
            recorded: false,
        },
    ];
    for case in cases {
        let mut txn = pool.begin().await?;
        let interface_id = create_interface(&mut txn).await?;
        let address: std::net::IpAddr = case.address.parse()?;
        let family = address.address_family();
        addresses::insert(&mut txn, interface_id, address, case.allocation_type).await?;
        match case.removal {
            Removal::Address => {
                addresses::delete_by_address(&mut txn, address, case.allocation_type).await?;
            }
            Removal::Mac | Removal::WrongMac => {
                let mac_address: MacAddress = if matches!(case.removal, Removal::WrongMac) {
                    "02:00:00:00:00:02"
                } else {
                    "02:00:00:00:00:01"
                }
                .parse()?;
                addresses::delete_by_address_and_mac(
                    &mut txn,
                    address,
                    mac_address,
                    case.allocation_type,
                )
                .await?;
            }
            Removal::Family => {
                addresses::delete_by_interface_family(
                    &mut txn,
                    interface_id,
                    family,
                    case.allocation_type,
                )
                .await?;
            }
            Removal::InterfaceAddress => {
                addresses::delete_by_interface_and_address(
                    &mut txn,
                    interface_id,
                    address,
                    case.allocation_type,
                )
                .await?;
            }
        }
        let recorded: (bool, bool) = sqlx::query_as("SELECT ipv4_allocation_removed, ipv6_allocation_removed FROM machine_interfaces WHERE id = $1")
            .bind(interface_id).fetch_one(&mut *txn).await?;
        let expected = match family {
            IpAddressFamily::Ipv4 => (case.recorded, false),
            IpAddressFamily::Ipv6 => (false, case.recorded),
        };
        assert_eq!(recorded, expected, "{}", case.name);
        let sibling = match family {
            IpAddressFamily::Ipv4 => IpAddressFamily::Ipv6,
            IpAddressFamily::Ipv6 => IpAddressFamily::Ipv4,
        };
        assert!(
            can_apply_expected_allocation(&mut txn, interface_id, sibling).await?,
            "{}",
            case.name
        );
        assert_eq!(
            can_apply_expected_allocation(&mut txn, interface_id, family).await?,
            case.allocation_type == AllocationType::Slaac,
            "{}",
            case.name
        );
        txn.rollback().await?;
    }
    Ok(())
}

#[crate::sqlx_test]
async fn removal_evidence_rolls_back_with_the_address(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = crate::Transaction::begin(&pool).await?;
    let interface_id = create_interface(&mut txn).await?;
    addresses::insert(
        &mut txn,
        interface_id,
        "192.0.2.1".parse()?,
        AllocationType::Dhcp,
    )
    .await?;
    let mut removal = crate::Transaction::begin_inner(&mut txn).await?;
    addresses::delete_by_interface_family(
        &mut removal,
        interface_id,
        IpAddressFamily::Ipv4,
        AllocationType::Dhcp,
    )
    .await?;
    removal.rollback().await?;
    let recorded: bool =
        sqlx::query_scalar("SELECT ipv4_allocation_removed FROM machine_interfaces WHERE id = $1")
            .bind(interface_id)
            .fetch_one(txn.as_pgconn())
            .await?;
    assert!(!recorded);
    assert_eq!(
        addresses::find_for_interface(&mut txn, interface_id)
            .await?
            .len(),
        1
    );
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn migration_preserves_addresses_and_does_not_guess_removals(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    sqlx::raw_sql("ALTER TABLE machine_interfaces DROP COLUMN ipv4_allocation_removed, DROP COLUMN ipv6_allocation_removed")
        .execute(&mut *txn).await?;
    let interface_id = create_interface(&mut txn).await?;
    addresses::insert(
        &mut txn,
        interface_id,
        "192.0.2.1".parse()?,
        AllocationType::Dhcp,
    )
    .await?;
    addresses::insert(
        &mut txn,
        interface_id,
        "2001:db8::1".parse()?,
        AllocationType::Slaac,
    )
    .await?;
    sqlx::query("UPDATE machine_interfaces SET last_dhcp = now() WHERE id = $1")
        .bind(interface_id)
        .execute(&mut *txn)
        .await?;
    sqlx::raw_sql(REMOVAL_MIGRATION).execute(&mut *txn).await?;
    let recorded: (bool, bool) = sqlx::query_as("SELECT ipv4_allocation_removed, ipv6_allocation_removed FROM machine_interfaces WHERE id = $1")
        .bind(interface_id).fetch_one(&mut *txn).await?;
    assert_eq!(recorded, (false, false));
    assert!(!can_apply_expected_allocation(&mut txn, interface_id, IpAddressFamily::Ipv4).await?);
    assert!(can_apply_expected_allocation(&mut txn, interface_id, IpAddressFamily::Ipv6).await?);
    assert_eq!(
        addresses::find_for_interface(&mut txn, interface_id)
            .await?
            .len(),
        2
    );
    txn.rollback().await?;
    Ok(())
}
