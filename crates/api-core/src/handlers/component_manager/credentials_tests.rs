// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ::rpc::forge::forge_server::Forge;
use carbide_authn::middleware::Principal;
use carbide_instrument::testing::capture_logs_async;
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::CompositeCredentialManager;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use db::test_support::postgres::wait_for_blocked_query;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::expected_power_shelf::ExpectedPowerShelf;
use model::expected_switch::ExpectedSwitch;
use model::machine::CURRENT_STATE_MODEL_VERSION;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::power_shelf::NewPowerShelf;
use model::switch::{NewSwitch, SwitchConfig};
use model::test_support::power_shelf_config;

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

/// Password of every expected-inventory row the fixtures below insert.
const EXPECTED_PASSWORD: &str = "expected-pw";

/// Inserts an expected power shelf whose BMC username is `name`, and, unless
/// `ingested` is false, its power-shelf row.
async fn create_shelf(
    txn: &mut sqlx::PgConnection,
    name: &str,
    mac: MacAddress,
    ingested: bool,
) -> Option<model::power_shelf::PowerShelf> {
    db::expected_power_shelf::create(
        &mut *txn,
        ExpectedPowerShelf {
            bmc_mac_address: mac,
            bmc_username: name.into(),
            bmc_password: EXPECTED_PASSWORD.into(),
            serial_number: name.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    if !ingested {
        return None;
    }
    Some(
        db::power_shelf::create(
            txn,
            &NewPowerShelf {
                id: PowerShelfId::from(uuid::Uuid::new_v4()),
                config: power_shelf_config(name),
                bmc_mac_address: Some(mac),
                metadata: None,
                rack_id: None,
            },
        )
        .await
        .unwrap(),
    )
}

/// Inserts an expected switch whose BMC username is `name`, and, unless
/// `ingested` is false, its switch row.
async fn create_switch(
    txn: &mut sqlx::PgConnection,
    name: &str,
    mac: MacAddress,
    ingested: bool,
) -> Option<model::switch::Switch> {
    db::expected_switch::create(
        &mut *txn,
        ExpectedSwitch {
            bmc_mac_address: mac,
            bmc_username: name.into(),
            bmc_password: EXPECTED_PASSWORD.into(),
            serial_number: name.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    if !ingested {
        return None;
    }
    Some(
        db::switch::create(
            txn,
            &NewSwitch {
                id: SwitchId::from(uuid::Uuid::new_v4()),
                config: SwitchConfig {
                    name: name.into(),
                    enable_nmxc: false,
                    fabric_manager_config: None,
                },
                bmc_mac_address: Some(mac),
                metadata: None,
                rack_id: None,
                slot_number: None,
                tray_index: None,
            },
        )
        .await
        .unwrap(),
    )
}

#[crate::sqlx_test]
async fn component_credentials_are_scoped_to_ingested_components(pool: sqlx::PgPool) {
    use ::rpc::forge::ComponentCredentialKind::{
        PowerShelfPmc, SwitchBmc, SwitchNvos, Unspecified,
    };

    // The service identity a row calls with; `None` is a caller without one.
    const PSM: Option<&str> = Some("nico-powershelf-manager");
    const NSM: Option<&str> = Some("nico-nvswitch-manager");
    let env = crate::tests::create_test_env(pool.clone()).await;
    let mac = |last: u8| MacAddress::new([0x02, 0, 0, 0, 0x0a, last]);
    let stored_shelf = mac(0xbc);
    let unstored_shelf = mac(0x01);
    let expected_only_shelf = mac(0x02);
    let deleted_shelf = mac(0x03);
    let machine_owned_shelf = mac(0x04);
    let stored_switch = mac(0xcd);
    let expected_only_switch = mac(0x05);
    let deleted_switch = mac(0x06);
    let switch_without_nvos = mac(0x07);

    let mut txn = pool.begin().await.unwrap();
    create_shelf(&mut txn, "stored-shelf", stored_shelf, true).await;
    create_shelf(&mut txn, "unstored-shelf", unstored_shelf, true).await;
    create_shelf(&mut txn, "exp-only-shelf", expected_only_shelf, false).await;
    let mut deleted = create_shelf(&mut txn, "deleted-shelf", deleted_shelf, true)
        .await
        .unwrap();
    db::power_shelf::mark_as_deleted(&mut deleted, &mut txn)
        .await
        .unwrap();
    create_shelf(&mut txn, "machine-shelf", machine_owned_shelf, true).await;
    create_switch(&mut txn, "stored-switch", stored_switch, true).await;
    create_switch(&mut txn, "exp-only-switch", expected_only_switch, false).await;
    let mut deleted_switch_row = create_switch(&mut txn, "deleted-switch", deleted_switch, true)
        .await
        .unwrap();
    db::switch::mark_as_deleted(&mut deleted_switch_row, &mut txn)
        .await
        .unwrap();
    // Ingested, with no NVOS pair in its expected row and none stored.
    create_switch(&mut txn, "no-nvos-switch", switch_without_nvos, true).await;
    // A machine owns an interface with the same MAC address as a power shelf.
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [7; 32],
        MachineType::Host,
    );
    db::machine::create(
        &mut txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        Some(&ExpectedMachineData::default()),
        CURRENT_STATE_MODEL_VERSION,
    )
    .await
    .unwrap();
    let underlay = db::network_segment::find_by_name(&mut txn, "UNDERLAY")
        .await
        .unwrap();
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&underlay),
        &machine_owned_shelf,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap();
    db::machine_interface::associate_bmc_interface(
        &interface.id,
        MachineInterfaceAssociation::Machine(machine_id),
        &mut txn,
    )
    .await
    .unwrap();
    // An interface with a power shelf's MAC address that no machine owns does
    // not refuse the read.
    db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&underlay),
        &unstored_shelf,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Every membership or identity refusal below is for a MAC address with a
    // stored credential that the resolver would return, so such a row cannot
    // pass because nothing is stored.
    let bmc_root = |bmc_mac_address| CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
    };
    for (key, user) in [
        (bmc_root(stored_shelf), "stored-shelf"),
        (bmc_root(expected_only_shelf), "stored-host"),
        (bmc_root(deleted_shelf), "stored-deleted-shelf"),
        (bmc_root(machine_owned_shelf), "stored-machine"),
        (bmc_root(stored_switch), "stored-switch-bmc"),
        (bmc_root(expected_only_switch), "stored-host"),
        (bmc_root(deleted_switch), "stored-deleted-switch"),
        (
            CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: stored_switch,
            },
            "stored-switch-nvos",
        ),
    ] {
        env.api
            .credential_manager
            .set_credentials(&key, &Credentials::new(user, format!("{user}-password")))
            .await
            .unwrap();
    }

    // The request as a caller with this service identity sends it.
    let request = |caller: Option<&str>, kind: i32, bmc_mac_address: &str| {
        let mut request = Request::new(rpc::GetComponentCredentialsRequest {
            bmc_mac_address: bmc_mac_address.to_string(),
            kind,
        });
        let mut context = AuthContext::default();
        if let Some(caller) = caller {
            context
                .principals
                .push(Principal::SpiffeServiceIdentifier(caller.to_string()));
        }
        request.extensions_mut().insert(context);
        request
    };
    let stored = |user: &str| Ok((user.to_string(), format!("{user}-password")));
    for (label, caller, kind, bmc_mac_address, want) in [
        (
            "power shelf with a store entry, lowercase MAC",
            PSM,
            PowerShelfPmc,
            "02:00:00:00:0a:bc".to_string(),
            stored("stored-shelf"),
        ),
        (
            "switch BMC",
            NSM,
            SwitchBmc,
            stored_switch.to_string(),
            stored("stored-switch-bmc"),
        ),
        (
            "switch NVOS",
            NSM,
            SwitchNvos,
            stored_switch.to_string(),
            stored("stored-switch-nvos"),
        ),
        (
            "ingested power shelf without a store entry, on an unowned interface",
            PSM,
            PowerShelfPmc,
            unstored_shelf.to_string(),
            Ok(("unstored-shelf".to_string(), EXPECTED_PASSWORD.to_string())),
        ),
        (
            "expected power shelf without a power-shelf row",
            PSM,
            PowerShelfPmc,
            expected_only_shelf.to_string(),
            Err(Code::NotFound),
        ),
        (
            "expected switch without a switch row",
            NSM,
            SwitchBmc,
            expected_only_switch.to_string(),
            Err(Code::NotFound),
        ),
        (
            "soft-deleted power shelf",
            PSM,
            PowerShelfPmc,
            deleted_shelf.to_string(),
            Err(Code::NotFound),
        ),
        (
            "soft-deleted switch",
            NSM,
            SwitchBmc,
            deleted_switch.to_string(),
            Err(Code::NotFound),
        ),
        (
            "power shelf whose MAC a machine also owns",
            PSM,
            PowerShelfPmc,
            machine_owned_shelf.to_string(),
            Err(Code::NotFound),
        ),
        (
            "switch BMC kind for the MAC of an ingested power shelf",
            NSM,
            SwitchBmc,
            stored_shelf.to_string(),
            Err(Code::NotFound),
        ),
        (
            "PMC kind for the MAC of an ingested switch",
            PSM,
            PowerShelfPmc,
            stored_switch.to_string(),
            Err(Code::NotFound),
        ),
        (
            "ingested switch with no NVOS credential stored or expected",
            NSM,
            SwitchNvos,
            switch_without_nvos.to_string(),
            Err(Code::NotFound),
        ),
        (
            "switch kind requested by the power-shelf manager",
            PSM,
            SwitchBmc,
            stored_switch.to_string(),
            Err(Code::PermissionDenied),
        ),
        (
            "PMC kind requested by the switch manager",
            NSM,
            PowerShelfPmc,
            stored_shelf.to_string(),
            Err(Code::PermissionDenied),
        ),
        (
            "PMC kind requested by a service that is not a component manager",
            Some("nico-flow"),
            PowerShelfPmc,
            stored_shelf.to_string(),
            Err(Code::PermissionDenied),
        ),
        (
            "PMC kind requested by a caller with no service identity",
            None,
            PowerShelfPmc,
            stored_shelf.to_string(),
            Err(Code::PermissionDenied),
        ),
        (
            "unspecified kind",
            PSM,
            Unspecified,
            stored_shelf.to_string(),
            Err(Code::InvalidArgument),
        ),
        (
            "malformed MAC address",
            PSM,
            PowerShelfPmc,
            "not-a-mac".to_string(),
            Err(Code::InvalidArgument),
        ),
    ] {
        let request = request(caller, kind as i32, &bmc_mac_address);
        let (response, logs) = capture_logs_async(env.api.get_component_credentials(request)).await;
        let got = response
            .map(|response| {
                let credentials = response.into_inner().credentials.unwrap();
                (credentials.username, credentials.password)
            })
            .map_err(|status| status.code());
        assert_eq!(got, want, "{label}");

        // The handler's own refusal is the only outcome that logs this
        // warning, and the line names the identity that asked.
        let refusals: Vec<_> = logs
            .iter()
            .filter(|log| {
                log.level == tracing::Level::WARN
                    && log.message
                        == "component credential request refused for this service identity"
            })
            .map(|log| {
                (
                    log.field("caller_service_id"),
                    log.field("permitted_service_id"),
                    log.field("rack_component"),
                    log.field("management_mac_address"),
                )
            })
            .collect();
        let want_refusals = if want == Err(Code::PermissionDenied) {
            let (permitted, component) = match kind {
                PowerShelfPmc => (PSM, "PowerShelfPmc"),
                SwitchBmc => (NSM, "SwitchBmc"),
                SwitchNvos => (NSM, "SwitchNvos"),
                Unspecified => unreachable!("an unspecified kind is refused as invalid"),
            };
            vec![(
                caller,
                permitted,
                Some(component),
                Some(bmc_mac_address.as_str()),
            )]
        } else {
            Vec::new()
        };
        assert_eq!(refusals, want_refusals, "{label}");

        // Neither a stored nor an expected-inventory password is logged.
        let logged_password = logs.iter().any(|log| {
            std::iter::once(&log.message)
                .chain(log.fields.iter().map(|(_, value)| value))
                .any(|text| text.contains("-password") || text.contains(EXPECTED_PASSWORD))
        });
        assert!(!logged_password, "{label}: a password reached the log");
    }

    // A number that names no kind is refused like an unspecified one.
    let unknown_kind = env
        .api
        .get_component_credentials(request(PSM, 99, &stored_shelf.to_string()))
        .await
        .unwrap_err();
    assert_eq!(unknown_kind.code(), Code::InvalidArgument);
}

/// A credential-store failure is reported as `Internal` and the store's error
/// text does not reach the caller.
#[crate::sqlx_test]
async fn component_credentials_store_failure_is_internal_without_detail(pool: sqlx::PgPool) {
    let env = crate::tests::create_test_env(pool.clone()).await;
    let shelf = MacAddress::new([0x02, 0, 0, 0, 0x0b, 0x01]);
    let mut txn = pool.begin().await.unwrap();
    create_shelf(&mut txn, "failing-shelf", shelf, true).await;
    txn.commit().await.unwrap();

    let api = TestApiBuilder::new(
        pool.clone(),
        env.api.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_credential_manager(Arc::new(CompositeCredentialManager::new(
        StubCredentialReader {
            result: Err(()),
            reads: Arc::new(AtomicUsize::new(0)),
        },
        TestCredentialManager::default(),
    )))
    .build();

    let mut request = Request::new(rpc::GetComponentCredentialsRequest {
        bmc_mac_address: shelf.to_string(),
        kind: rpc::ComponentCredentialKind::PowerShelfPmc as i32,
    });
    let mut context = AuthContext::default();
    context.principals.push(Principal::SpiffeServiceIdentifier(
        "nico-powershelf-manager".to_string(),
    ));
    request.extensions_mut().insert(context);

    let status = api.get_component_credentials(request).await.unwrap_err();
    assert_eq!(status.code(), Code::Internal);
    assert!(
        !status.message().contains("test vault read failure"),
        "store error text reached the caller: {}",
        status.message()
    );
}

/// Builds an API that reads through a pool of one connection, so that a
/// session a read keeps or loses shows as the pool's capacity.
async fn one_connection_api(api: &Api, pool: &sqlx::PgPool) -> eyre::Result<(Api, sqlx::PgPool)> {
    let reader_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let reader_api = TestApiBuilder::new(
        reader_pool.clone(),
        api.common_pools.clone(),
        api.work_lock_manager_handle.clone(),
    )
    .build();
    Ok((reader_api, reader_pool))
}

/// Returns the PID of `reader_pool`'s only session, with no server statement
/// timeout that could end a blocked read by itself.
async fn reader_session(reader_pool: &sqlx::PgPool) -> eyre::Result<i32> {
    let (configured_timeout, pid): (String, i32) =
        sqlx::query_as("SELECT set_config('statement_timeout', '0', false), pg_backend_pid()")
            .fetch_one(reader_pool)
            .await?;
    assert_eq!(configured_timeout, "0");
    Ok(pid)
}

/// The device checks must release their local pool slot when the caller
/// abandons a blocked read, without waiting for the server statement cap or
/// the lock, and must return a healthy session before the handler answers or
/// resolves the credential. Both checks share one guarded session.
#[crate::sqlx_test]
async fn component_credentials_cancellation_releases_pool_capacity(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = crate::tests::create_test_env(pool.clone()).await;
    let shelf = MacAddress::new([2, 0, 0, 0, 0x0c, 1]);
    let unknown = MacAddress::new([2, 0, 0, 0, 0x0c, 2]);
    let mut txn = pool.begin().await?;
    create_shelf(&mut txn, "pool-shelf", shelf, true).await;
    txn.commit().await?;

    let (api, reader_pool) = one_connection_api(&env.api, &pool).await?;
    let request = |mac: MacAddress| {
        let mut request = Request::new(rpc::GetComponentCredentialsRequest {
            bmc_mac_address: mac.to_string(),
            kind: rpc::ComponentCredentialKind::PowerShelfPmc as i32,
        });
        let mut context = AuthContext::default();
        context.principals.push(Principal::SpiffeServiceIdentifier(
            "nico-powershelf-manager".to_string(),
        ));
        request.extensions_mut().insert(context);
        request
    };

    // A refusal is a successful read, so the handler must commit and return
    // this healthy session before answering `NotFound`.
    let reader_pid = reader_session(&reader_pool).await?;
    let refused = api
        .get_component_credentials(request(unknown))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::NotFound);
    assert_eq!(reader_session(&reader_pool).await?, reader_pid);

    // The expected-inventory lookup borrows a session of its own, so the
    // membership session must be back in the one-connection pool before it.
    let resolved = tokio::time::timeout(
        Duration::from_secs(2),
        api.get_component_credentials(request(shelf)),
    )
    .await
    .expect("the membership session must be returned before the credential lookup")
    .map_err(|status| eyre::eyre!("credential read failed: {status}"))?
    .into_inner()
    .credentials
    .unwrap();
    assert_eq!(
        (resolved.username.as_str(), resolved.password.as_str()),
        ("pool-shelf", EXPECTED_PASSWORD)
    );
    assert_eq!(reader_session(&reader_pool).await?, reader_pid);

    let reader_pid = reader_session(&reader_pool).await?;
    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE power_shelves IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let mut read = Box::pin(api.get_component_credentials(request(shelf)));
    let blocked_pid = tokio::select! {
        result = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_blocked_query(&pool, blocker_pid, "FROM power_shelves"),
        ) => result.expect("device check must reach the held lock"),
        result = &mut read => panic!(
            "credential read finished before blocking: {:?}",
            result.map(|_| ()).map_err(|status| status.code())
        ),
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
        recovered.expect("abandoned read must free its local pool slot")?,
        1
    );
    Ok(())
}

/// The read's deadline covers the wait for a pooled connection, the device
/// checks and the credential lookup. When it expires the caller gets
/// `DeadlineExceeded`, the read's session is discarded with its transaction,
/// and the pool serves again. A read that finishes first is unaffected.
#[crate::sqlx_test]
async fn component_credentials_read_ends_at_its_deadline(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env = crate::tests::create_test_env(pool.clone()).await;
    let shelf = MacAddress::new([2, 0, 0, 0, 0x0d, 1]);
    let mut txn = pool.begin().await?;
    create_shelf(&mut txn, "deadline-shelf", shelf, true).await;
    txn.commit().await?;

    let (api, reader_pool) = one_connection_api(&env.api, &pool).await?;
    // The deadline is the receiver, so each step expires it at a chosen
    // point and no real time has to pass.
    let read = |expired: tokio::sync::oneshot::Receiver<()>| {
        read_component_credentials(
            &api,
            RackComponent::PowerShelfPmc,
            IngestedComponent::PowerShelf,
            shelf,
            async move {
                expired.await.ok();
            },
        )
    };
    // What a read answers after its deadline has expired: the status code,
    // its text, and the operator error code of the typed error.
    fn expired_answer(
        answer: Result<Result<Credentials, Status>, tokio::time::error::Elapsed>,
        step: &str,
    ) -> (Code, String, Option<String>) {
        match answer {
            Ok(Err(status)) => (
                status.code(),
                status.message().to_string(),
                status
                    .metadata()
                    .get("nico-error-code")
                    .and_then(|code| code.to_str().ok())
                    .map(str::to_string),
            ),
            Ok(Ok(_)) => panic!("{step}: the read resolved although its deadline had expired"),
            Err(_) => panic!("{step}: the read must answer when its deadline expires"),
        }
    }
    let deadline_exceeded = (
        Code::DeadlineExceeded,
        "component credential read timed out".to_string(),
        Some("NICO-API-504".to_string()),
    );

    let (unexpired, expired) = tokio::sync::oneshot::channel();
    let Credentials::UsernamePassword { username, password } = read(expired)
        .await
        .map_err(|status| eyre::eyre!("credential read failed: {status}"))?;
    assert_eq!(
        (username.as_str(), password.as_str()),
        ("deadline-shelf", EXPECTED_PASSWORD)
    );
    drop(unexpired);

    // With the pool's only connection in use, the read waits to acquire one.
    let reader_pid = reader_session(&reader_pool).await?;
    let held = reader_pool.acquire().await?;
    let (expire, expired) = tokio::sync::oneshot::channel();
    let mut stalled = Box::pin(read(expired));
    assert!(futures::poll!(&mut stalled).is_pending());
    expire.send(()).ok();
    let answer = tokio::time::timeout(Duration::from_secs(2), stalled).await;
    assert_eq!(
        expired_answer(answer, "pool acquisition"),
        deadline_exceeded
    );
    drop(held);
    assert_eq!(reader_session(&reader_pool).await?, reader_pid);

    for (step, lock_table, query_fragment) in [
        (
            "device check",
            "LOCK TABLE power_shelves IN ACCESS EXCLUSIVE MODE",
            "FROM power_shelves",
        ),
        (
            "expected-inventory read",
            "LOCK TABLE expected_power_shelves IN ACCESS EXCLUSIVE MODE",
            "FROM expected_power_shelves",
        ),
    ] {
        let reader_pid = reader_session(&reader_pool).await?;
        let mut blocker = pool.begin().await?;
        sqlx::query(lock_table).execute(&mut *blocker).await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let (expire, expired) = tokio::sync::oneshot::channel();
        let mut stalled = Box::pin(read(expired));
        let blocked_pid = tokio::select! {
            result = tokio::time::timeout(
                Duration::from_secs(2),
                wait_for_blocked_query(&pool, blocker_pid, query_fragment),
            ) => result.unwrap_or_else(|_| panic!("{step} must reach the held lock")),
            result = &mut stalled => panic!(
                "{step}: credential read finished before blocking: {:?}",
                result.map(|_| ()).map_err(|status| status.code())
            ),
        };
        assert_eq!(blocked_pid, reader_pid, "{step}");

        expire.send(()).ok();
        let answer = tokio::time::timeout(Duration::from_secs(2), stalled).await;
        assert_eq!(expired_answer(answer, step), deadline_exceeded, "{step}");

        // The session was discarded, not returned: the pool is empty, and it
        // serves a new borrow while the table remains locked.
        assert_eq!(reader_pool.size(), 0, "{step}");
        let recovered = tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&reader_pool),
        )
        .await;
        blocker.rollback().await?;
        assert_eq!(
            recovered
                .unwrap_or_else(|_| panic!("{step}: an expired read must free its pool slot"))?,
            1
        );

        // Released from the lock, the abandoned session ends, and with it the
        // transaction the read had open.
        tokio::time::timeout(Duration::from_secs(2), async {
            while sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid = $1)",
            )
            .bind(reader_pid)
            .fetch_one(&pool)
            .await?
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            eyre::Ok(())
        })
        .await
        .unwrap_or_else(|_| panic!("{step}: the abandoned session must end"))?;
    }
    reader_pool.close().await;
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
