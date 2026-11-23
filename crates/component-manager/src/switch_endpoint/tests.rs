// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::{IpAddr, Ipv4Addr};

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, CredentialWriter, Credentials,
};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use db::TransactionVending as _;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
use model::machine_interface::InterfaceType;

use super::resolve_pre_ingestion_switch_endpoints;
use crate::error::ComponentManagerError;

#[carbide_macros::sqlx_test]
async fn pre_ingestion_switch_endpoints_preserve_credentials_and_partial_batch_results(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.txn_begin().await?;
    let segment: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('resolver-underlay', 'V1-T0', 'underlay') RETURNING id",
    )
    .fetch_one(txn.as_mut())
    .await?;

    let mac = |index| MacAddress::new([2, 0, 0, 0, 1, index]);
    let nvos_mac = |index| MacAddress::new([2, 0, 0, 0, 2, index]);
    let bmc_ip = |index| IpAddr::V4(Ipv4Addr::new(10, 40, 1, index));
    let nvos_ip = |index| IpAddr::V4(Ipv4Addr::new(10, 40, 2, index));

    // All four switches have expected inventory and allocated BMC addresses,
    // but none has a managed switch row. The two incomplete entries must not
    // prevent either complete entry from resolving in the same batch.
    for index in 1..=4 {
        db::expected_switch::create(
            &mut txn,
            ExpectedSwitch {
                bmc_mac_address: mac(index),
                serial_number: format!("resolver-switch-{index}"),
                bmc_username: "factory-bmc".into(),
                bmc_password: "factory-bmc-password".into(),
                nvos_username: Some("factory-nvos".into()),
                nvos_password: (index != 3).then(|| "factory-nvos-password".into()),
                nvos_interfaces: vec![ExpectedNvosInterface {
                    mac_address: nvos_mac(index),
                    is_primary: true,
                }],
                ..Default::default()
            },
        )
        .await?;
        insert_interface(
            &mut txn,
            segment,
            mac(index),
            Some(bmc_ip(index)),
            InterfaceType::Bmc,
        )
        .await?;
        insert_interface(
            &mut txn,
            segment,
            nvos_mac(index),
            (index != 2).then(|| nvos_ip(index)),
            InterfaceType::Data,
        )
        .await?;
    }
    txn.commit().await?;

    let credentials = TestCredentialManager::default();
    let bmc_key = |bmc_mac_address| CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
    };
    let nvos_key = |bmc_mac_address| CredentialKey::SwitchNvosAdmin { bmc_mac_address };
    let vault_bmc = Credentials::new("vault-bmc", "vault-bmc-password");
    let vault_nvos = Credentials::new("vault-nvos", "vault-nvos-password");
    credentials
        .set_credentials(&bmc_key(mac(4)), &vault_bmc)
        .await
        .expect("seed stored BMC credentials");
    credentials
        .set_credentials(&nvos_key(mac(4)), &vault_nvos)
        .await
        .expect("seed stored NVOS credentials");

    // Deliberately interleave failures and successes, rather than MAC order.
    let requested = [mac(2), mac(4), mac(99), mac(1), mac(3)];
    let results =
        resolve_pre_ingestion_switch_endpoints(&pool, &credentials, &requested, None).await?;
    assert_eq!(
        results
            .iter()
            .map(|result| result.bmc_mac)
            .collect::<Vec<_>>(),
        requested,
        "every requested MAC must retain its position, including failures"
    );

    for (position, index, expected_bmc, expected_nvos) in [
        (1, 4, vault_bmc.clone(), vault_nvos.clone()),
        (
            3,
            1,
            Credentials::new("factory-bmc", "factory-bmc-password"),
            Credentials::new("factory-nvos", "factory-nvos-password"),
        ),
    ] {
        let endpoint = results[position].endpoint.as_ref().unwrap();
        assert_eq!(endpoint.bmc_mac, mac(index));
        assert_eq!(endpoint.bmc_ip, bmc_ip(index));
        assert_eq!(endpoint.nvos_mac, nvos_mac(index));
        assert_eq!(endpoint.nvos_ip, nvos_ip(index));
        assert_eq!(endpoint.bmc_credentials, expected_bmc);
        assert_eq!(endpoint.nvos_credentials, expected_nvos);
    }

    for (position, label) in [
        (0, "missing NVOS address"),
        (2, "unknown expected switch"),
        (4, "missing NVOS password"),
    ] {
        assert!(
            matches!(
                results[position].endpoint,
                Err(ComponentManagerError::NotFound(_))
            ),
            "{label} must remain a per-MAC not-found error"
        );
    }

    // Resolution may fall back to inventory but must never publish its factory
    // credentials to Vault or replace credentials already managed there.
    for key in [bmc_key(mac(1)), nvos_key(mac(1))] {
        assert_eq!(
            credentials
                .get_credentials(&key)
                .await
                .expect("read missing per-device credentials"),
            None
        );
    }
    for (key, expected) in [(bmc_key(mac(4)), vault_bmc), (nvos_key(mac(4)), vault_nvos)] {
        assert_eq!(
            credentials
                .get_credentials(&key)
                .await
                .expect("read stored per-device credentials"),
            Some(expected)
        );
    }
    Ok(())
}

async fn insert_interface(
    txn: &mut sqlx::PgConnection,
    segment: NetworkSegmentId,
    mac: MacAddress,
    address: Option<IpAddr>,
    interface_type: InterfaceType,
) -> Result<(), Box<dyn std::error::Error>> {
    let id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
             (segment_id, mac_address, primary_interface, hostname, interface_type)
         VALUES ($1, $2, false, 'resolver-interface', $3) RETURNING id",
    )
    .bind(segment)
    .bind(mac)
    .bind(interface_type)
    .fetch_one(&mut *txn)
    .await?;
    if let Some(address) = address {
        db::machine_interface_address::insert(txn, id, address, AllocationType::Dhcp).await?;
    }
    Ok(())
}
