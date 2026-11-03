// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use carbide_secrets::SecretsError;
use carbide_secrets::credentials::CompositeCredentialManager;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::expected_power_shelf::ExpectedPowerShelf;
use model::expected_switch::ExpectedSwitch;

use super::*;
use crate::test_support::builder::TestApiBuilder;

struct StubCredentialReader(Result<Option<Credentials>, ()>);

#[async_trait::async_trait]
impl CredentialReader for StubCredentialReader {
    async fn get_credentials(
        &self,
        _key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.0
            .clone()
            .map_err(|()| SecretsError::GenericError(eyre::eyre!("test vault read failure")))
    }
}

#[crate::sqlx_test]
async fn direct_dispatch_credentials_only_query_expected_inventory_on_vault_miss(
    db_pool: sqlx::PgPool,
) {
    let env = crate::tests::create_test_env(db_pool).await;
    let pool = sqlx::PgPool::connect_lazy("postgresql://localhost/unused").unwrap();
    pool.close().await;
    let vault = Credentials::new("vault-user", "vault-password");
    for (label, vault_result, want) in [
        (
            "Vault hit skips database",
            Ok(Some(vault.clone())),
            Ok(vault),
        ),
        ("Vault error skips database", Err(()), Err("vault")),
        (
            "Vault miss propagates database failure",
            Ok(None),
            Err("database"),
        ),
    ] {
        let api = TestApiBuilder::new(
            pool.clone(),
            env.api.common_pools.clone(),
            env.api.work_lock_manager_handle.clone(),
        )
        .with_credential_manager(Arc::new(CompositeCredentialManager::new(
            StubCredentialReader(vault_result),
            TestCredentialManager::default(),
        )))
        .build();
        let result = fetch_credentials(
            &api,
            RackComponent::SwitchNvos,
            MacAddress::new([2, 0, 0, 0, 0, 1]),
        )
        .await
        .map_err(|error| match error {
            ComponentManagerError::Internal(message)
                if message.contains("test vault read failure") =>
            {
                "vault"
            }
            ComponentManagerError::Internal(message)
                if message.contains("failed to fetch expected credentials") =>
            {
                "database"
            }
            other => panic!("{label}: unexpected error {other}"),
        });
        assert_eq!(result, want, "{label}");
    }
}

#[crate::sqlx_test]
async fn direct_dispatch_credentials_use_the_matching_expected_component(pool: sqlx::PgPool) {
    // Distinct credentials at the same MAC prove each kind selects its own table.
    let mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
    let mut conn = pool.acquire().await.unwrap();
    db::expected_machine::create(
        &mut conn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: mac,
            data: ExpectedMachineData {
                bmc_username: "compute".into(),
                bmc_password: "compute-password".into(),
                serial_number: "compute-serial".into(),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    db::expected_switch::create(
        &mut conn,
        ExpectedSwitch {
            bmc_mac_address: mac,
            bmc_username: "switch".into(),
            bmc_password: "switch-password".into(),
            nvos_username: Some("nvos".into()),
            nvos_password: Some("nvos-password".into()),
            serial_number: "switch-serial".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    db::expected_power_shelf::create(
        &mut conn,
        ExpectedPowerShelf {
            bmc_mac_address: mac,
            bmc_username: "shelf".into(),
            bmc_password: "shelf-password".into(),
            serial_number: "shelf-serial".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    drop(conn);

    for (user, kind) in [
        ("compute", RackComponent::ComputeBmc),
        ("switch", RackComponent::SwitchBmc),
        ("nvos", RackComponent::SwitchNvos),
        ("shelf", RackComponent::PowerShelfPmc),
    ] {
        assert_eq!(
            fetch_expected_credentials(kind, &pool, mac).await.unwrap(),
            Credentials::new(user, format!("{user}-password")),
            "{user}",
        );
    }
    assert!(matches!(
        fetch_expected_credentials(
            RackComponent::ComputeBmc,
            &pool,
            MacAddress::new([2, 0, 0, 0, 0, 4])
        )
        .await,
        Err(ComponentManagerError::NotFound(_))
    ));
}

#[crate::sqlx_test]
async fn direct_dispatch_credentials_require_both_expected_nvos_fields(pool: sqlx::PgPool) {
    for (index, (username, password)) in [
        (Some("nvos".into()), None),
        (None, Some("nvos-password".into())),
    ]
    .into_iter()
    .enumerate()
    {
        let mac = MacAddress::new([2, 0, 0, 0, 0, index as u8]);
        let mut conn = pool.acquire().await.unwrap();
        db::expected_switch::create(
            &mut conn,
            ExpectedSwitch {
                bmc_mac_address: mac,
                bmc_username: "bmc".into(),
                bmc_password: "bmc-password".into(),
                nvos_username: username,
                nvos_password: password,
                serial_number: format!("switch-{index}"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        drop(conn);
        assert!(matches!(
            fetch_expected_credentials(RackComponent::SwitchNvos, &pool, mac).await,
            Err(ComponentManagerError::NotFound(_))
        ));
    }
}
