// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use carbide_secrets::SecretsError;
use carbide_secrets::credentials::CompositeCredentialManager;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use db::test_support::postgres::wait_for_blocked_query;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::expected_power_shelf::ExpectedPowerShelf;
use model::expected_switch::ExpectedSwitch;

use super::*;
use crate::test_support::builder::TestApiBuilder;

struct StubCredentialReader {
    result: Result<Option<Credentials>, ()>,
    reads: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl CredentialReader for StubCredentialReader {
    async fn get_credentials(
        &self,
        _key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.result
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
    let overrides = |bmc, nvos| Some(rpc::ComponentCredentialOverrides { bmc, nvos });
    use RackComponent::{ComputeBmc, SwitchNvos};
    for (label, component, overrides, vault_result, want, expected_reads) in [
        (
            "Vault hit skips database",
            SwitchNvos,
            None,
            Ok(Some(vault.clone())),
            Ok(vault),
            1,
        ),
        (
            "Vault error skips database",
            SwitchNvos,
            None,
            Err(()),
            Err("vault"),
            1,
        ),
        (
            "Vault miss propagates database failure",
            SwitchNvos,
            None,
            Ok(None),
            Err("database"),
            1,
        ),
        (
            "override skips unavailable credential stores",
            SwitchNvos,
            overrides(None, Some(pair("request-nvos"))),
            Err(()),
            Ok(Credentials::new("request-nvos", "request-nvos-password")),
            0,
        ),
        (
            "override wins over Vault",
            SwitchNvos,
            overrides(None, Some(pair("request-nvos"))),
            Ok(Some(Credentials::new("vault", "vault-password"))),
            Ok(Credentials::new("request-nvos", "request-nvos-password")),
            0,
        ),
        (
            "BMC override leaves NVOS lookup unchanged",
            SwitchNvos,
            overrides(Some(pair("request-bmc")), None),
            Ok(Some(Credentials::new("vault-nvos", "vault-password"))),
            Ok(Credentials::new("vault-nvos", "vault-password")),
            1,
        ),
        (
            "NVOS override leaves BMC lookup unchanged",
            ComputeBmc,
            overrides(None, Some(pair("request-nvos"))),
            Ok(Some(Credentials::new("vault-bmc", "vault-password"))),
            Ok(Credentials::new("vault-bmc", "vault-password")),
            1,
        ),
    ] {
        let reads = Arc::new(AtomicUsize::new(0));
        let credentials = parse_component_credentials(overrides).unwrap();
        let api = TestApiBuilder::new(
            pool.clone(),
            env.api.common_pools.clone(),
            env.api.work_lock_manager_handle.clone(),
        )
        .with_credential_manager(Arc::new(CompositeCredentialManager::new(
            StubCredentialReader {
                result: vault_result,
                reads: reads.clone(),
            },
            TestCredentialManager::default(),
        )))
        .build();
        let result = resolve_credentials(
            &api,
            component,
            MacAddress::new([2, 0, 0, 0, 0, 1]),
            credentials.as_ref(),
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
        assert_eq!(reads.load(Ordering::Relaxed), expected_reads, "{label}");
    }
}

#[crate::sqlx_test]
async fn direct_dispatch_credentials_use_the_matching_expected_component(pool: sqlx::PgPool) {
    // Distinct credentials at the same MAC prove each kind selects its own table.
    let mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
    let mut txn = pool.begin().await.unwrap();
    db::expected_machine::create(
        &mut txn,
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
        &mut txn,
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
        &mut txn,
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
    txn.commit().await.unwrap();

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

/// Expected-credential fallback must release its local pool slot when the caller
/// drops a blocked read, without waiting for the server statement cap or lock.
#[crate::sqlx_test]
async fn expected_credentials_cancellation_releases_pool_capacity(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let reader_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let (configured_timeout, reader_pid): (String, i32) =
        sqlx::query_as("SELECT set_config('statement_timeout', '0', false), pg_backend_pid()")
            .fetch_one(&reader_pool)
            .await?;
    assert_eq!(configured_timeout, "0");
    let mac = MacAddress::new([2, 0, 0, 0, 0, 9]);

    // Missing credentials are a successful query, so the helper must commit and
    // return this healthy session before mapping the absence to `NotFound`.
    let missing = fetch_expected_credentials(RackComponent::SwitchBmc, &reader_pool, mac).await;

    assert!(matches!(missing, Err(ComponentManagerError::NotFound(_))));
    let reused_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&reader_pool)
        .await?;
    assert_eq!(reused_pid, reader_pid);

    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE expected_switches IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let mut read = Box::pin(fetch_expected_credentials(
        RackComponent::SwitchBmc,
        &reader_pool,
        mac,
    ));
    let blocked_pid = tokio::select! {
        result = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_blocked_query(&pool, blocker_pid, "FROM expected_switches"),
        ) => result.expect("expected-credential query must reach the held lock"),
        result = &mut read => panic!("credential read finished before blocking: {result:?}"),
    };
    assert_eq!(blocked_pid, reader_pid);

    drop(read);

    // Check local capacity before any async cleanup can run, then borrow again
    // while the table remains locked. Unlocking first would hide blocked cleanup.
    assert_eq!(reader_pool.size(), 0);
    let recovered = tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&reader_pool),
    )
    .await;
    blocker.rollback().await?;
    reader_pool.close().await;
    assert_eq!(
        recovered.expect("cancelled read must free its local pool slot")?,
        1
    );
    Ok(())
}

fn pair(username: &str) -> rpc::UsernamePassword {
    rpc::UsernamePassword {
        username: username.into(),
        password: format!("{username}-password"),
    }
}

#[test]
fn credential_overrides_validate_presence_and_pairs() {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    let validate = |(bmc, nvos)| {
        parse_component_credentials(Some(rpc::ComponentCredentialOverrides { bmc, nvos }))
            .map(|_| ())
            .map_err(|error| error.code())
    };
    scenarios!(validate:
        "valid pairs" {
            (Some(pair("bmc")), None) => Yields(()),
            (None, Some(pair("nvos"))) => Yields(()),
            (Some(pair("bmc")), Some(pair("nvos"))) => Yields(()),
        }
        "invalid overrides" {
            (None, None) => FailsWith(Code::InvalidArgument),
            (Some(rpc::UsernamePassword { username: String::new(), ..pair("bmc") }), None) => FailsWith(Code::InvalidArgument),
            (None, Some(rpc::UsernamePassword { password: String::new(), ..pair("nvos") })) => FailsWith(Code::InvalidArgument),
        }
    );
}
