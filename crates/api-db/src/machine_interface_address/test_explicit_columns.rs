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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::dhcp_record::DhcpRecord;
use model::dns::NewDomain;
use sqlx::Connection;

use super::*;
use crate::{ObjectColumnFilter, dhcp_entry, dhcp_record};

#[crate::sqlx_test]
async fn interface_address_and_dhcp_entry_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    let mut txn = api_connection.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x78; 32],
        MachineType::Host,
    );
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}')")
        .bind(machine_id)
        .execute(txn.as_mut())
        .await?;
    let domain =
        crate::dns::domain::persist(NewDomain::new("projection.example"), txn.as_mut()).await?;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, subdomain_id, mtu)
         VALUES ('projection-segment', 'V1-T0', $1, 9000) RETURNING id",
    )
    .bind(domain.id)
    .fetch_one(txn.as_mut())
    .await?;
    let prefix = "192.0.2.0/24".parse::<ipnetwork::IpNetwork>()?;
    let gateway = "192.0.2.1".parse::<IpAddr>()?;
    sqlx::query(
        "INSERT INTO network_prefixes (segment_id, prefix, gateway, num_reserved)
         VALUES ($1, $2, $3, 1)",
    )
    .bind(segment_id)
    .bind(prefix)
    .bind(gateway)
    .execute(txn.as_mut())
    .await?;
    let mac_address: MacAddress = "02:00:00:00:00:78".parse()?;
    let interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
            (machine_id, segment_id, domain_id, mac_address, hostname, primary_interface,
             association_type)
         VALUES ($1, $2, $3, $4, 'projection-host', true, 'Machine') RETURNING id",
    )
    .bind(machine_id)
    .bind(segment_id)
    .bind(domain.id)
    .bind(mac_address)
    .fetch_one(txn.as_mut())
    .await?;
    let address: IpAddr = "192.0.2.78".parse()?;
    for address in [address, "2001:db8::78".parse()?] {
        insert(txn.as_mut(), interface_id, address, AllocationType::Dhcp).await?;
    }
    dhcp_entry::persist(
        dhcp_entry::DhcpEntry {
            machine_interface_id: interface_id,
            vendor_string: "PXEClient:Arch:00007".to_string(),
        },
        txn.as_mut(),
    )
    .await?;
    let expected = DhcpRecord {
        machine_id: Some(machine_id),
        segment_id,
        machine_interface_id: interface_id,
        subdomain_id: Some(domain.id),
        fqdn: "projection-host.projection.example".to_string(),
        mac_address,
        address,
        mtu: 9000,
        prefix,
        gateway: Some(gateway),
        last_invalidation_time: dhcp_record::last_invalidation_time(txn.as_mut()).await?,
    };
    txn.commit().await?;

    assert_address_and_dhcp_queries(&mut api_connection, &expected).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the prepared readers alive while unrelated columns are added.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE machine_interface_addresses ADD COLUMN test_added_column text;
         ALTER TABLE dhcp_entries ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    assert_address_and_dhcp_queries(&mut api_connection, &expected).await?;
    Ok(())
}

async fn assert_address_and_dhcp_queries(
    connection: &mut PgConnection,
    expected: &DhcpRecord,
) -> Result<(), Box<dyn std::error::Error>> {
    let address = find_ipv4_for_interface(connection, expected.machine_interface_id).await?;
    assert_eq!(address.address, expected.address);
    let entries = dhcp_entry::find_by(
        connection,
        ObjectColumnFilter::One(
            dhcp_entry::MachineInterfaceIdColumn,
            &expected.machine_interface_id,
        ),
    )
    .await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].machine_interface_id,
        expected.machine_interface_id
    );
    assert_eq!(entries[0].vendor_string, "PXEClient:Arch:00007");

    // The view has fixed result columns. This checks its decoder, not a
    // result expansion caused by adding a column to the underlying table.
    let record = dhcp_record::find_by_mac_address(
        connection,
        &expected.mac_address,
        &expected.segment_id,
        IpAddressFamily::Ipv4,
    )
    .await?
    .expect("the committed interface has a DHCP record");
    assert_eq!(record.machine_id, expected.machine_id);
    assert_eq!(record.segment_id, expected.segment_id);
    assert_eq!(record.machine_interface_id, expected.machine_interface_id);
    assert_eq!(record.subdomain_id, expected.subdomain_id);
    assert_eq!(record.fqdn, expected.fqdn);
    assert_eq!(record.mac_address, expected.mac_address);
    assert_eq!(record.address, expected.address);
    assert_eq!(record.mtu, expected.mtu);
    assert_eq!(record.prefix, expected.prefix);
    assert_eq!(record.gateway, expected.gateway);
    assert_eq!(
        record.last_invalidation_time,
        expected.last_invalidation_time
    );
    Ok(())
}
