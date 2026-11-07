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

//! Integration tests for the Vault-to-Postgres secrets import flow.
//! These tests start a real Vault dev server and connect to a real Postgres instance.
//! Requires: `vault` binary in PATH, `DATABASE_URL` env var set.

use std::str::FromStr;
use std::time::Duration;

use bmc_vendor::DpuModel;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, CredentialType, CredentialWriter,
    Credentials, MqttCredentialType, NicLockdownIkm,
};
use carbide_secrets::{VaultConfig, create_vault_client};
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use tokio::task::JoinSet;

/// make_test_key creates a deterministic 32-byte key from a seed byte.
fn make_test_key(seed: u8) -> [u8; 32] {
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = seed.wrapping_add(i as u8);
    }
    key
}

/// make_routing_and_kms creates a SecretRouting and IntegratedKmsProvider with a "/" catch-all.
fn make_routing_and_kms(
    key: [u8; 32],
) -> (
    carbide_api_core::secrets::SecretRouting,
    std::sync::Arc<dyn carbide_kms_provider::KmsBackend>,
) {
    let kek_id = "import-test-key".to_string();
    let mut keys = std::collections::HashMap::new();
    keys.insert(kek_id.clone(), key);
    let kms: std::sync::Arc<dyn carbide_kms_provider::KmsBackend> =
        std::sync::Arc::new(carbide_kms_provider::IntegratedKmsProvider::new(keys));
    let routing = carbide_api_core::secrets::SecretRouting::new(vec![("/".to_string(), kek_id)]);
    (routing, kms)
}

/// generate_test_secrets builds a set of CredentialKey/Credentials pairs covering every variant
/// shape, then scales up to at least `min_count` with synthetic dynamic IDs.
fn generate_test_secrets(min_count: usize) -> Vec<(CredentialKey, Credentials)> {
    let cred = |user: &str, pass: &str| Credentials::UsernamePassword {
        username: user.to_string(),
        password: pass.to_string(),
    };
    let mac = |i: u8| mac_address::MacAddress::new([0x00, 0x11, 0x22, 0x33, 0x44, i]);

    // Start with one of every static variant shape.
    let mut secrets: Vec<(CredentialKey, Credentials)> = vec![
        (
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRoot,
            },
            cred("bmc-root", "bmc-pass"),
        ),
        (
            CredentialKey::DpuRedfish {
                credential_type: CredentialType::SiteDefault,
            },
            cred("dpu-redfish-site", "pass"),
        ),
        (
            CredentialKey::DpuRedfish {
                credential_type: CredentialType::DpuHardwareDefault {
                    model: DpuModel::Unknown,
                },
            },
            cred("dpu-redfish-hw", "pass"),
        ),
        (
            CredentialKey::HostRedfish {
                credential_type: CredentialType::SiteDefault,
            },
            cred("host-redfish-site", "pass"),
        ),
        (
            CredentialKey::DpuUefi {
                credential_type: CredentialType::SiteDefault,
            },
            cred("dpu-uefi-site", "pass"),
        ),
        (
            CredentialKey::DpuUefi {
                credential_type: CredentialType::DpuHardwareDefault {
                    model: DpuModel::Unknown,
                },
            },
            cred("dpu-uefi-hw", "pass"),
        ),
        (
            CredentialKey::HostUefi {
                credential_type: CredentialType::SiteDefault,
            },
            cred("host-uefi-site", "pass"),
        ),
        (
            CredentialKey::MqttAuth {
                credential_type: MqttCredentialType::Dpa,
            },
            cred("mqtt-dpa", "pass"),
        ),
        (
            CredentialKey::MqttAuth {
                credential_type: MqttCredentialType::DsxExchangeEventBus,
            },
            cred("mqtt-event-bus", "pass"),
        ),
        (
            CredentialKey::MqttAuth {
                credential_type: MqttCredentialType::DsxExchangeConsumer,
            },
            cred("mqtt-consumer", "pass"),
        ),
    ];

    // Scale up with dynamic variants until we hit min_count.
    let mut i = 0u8;
    while secrets.len() < min_count {
        // Rotate through different dynamic variant types.
        let key = match i % 7 {
            0 => CredentialKey::UfmAuth {
                fabric: format!("fabric-{i}"),
            },
            1 => CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: mac(i),
                },
            },
            2 => CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcForgeAdmin {
                    bmc_mac_address: mac(i),
                },
            },
            3 => CredentialKey::ExtensionService {
                service_id: format!("svc-{i}"),
                version: format!("v{i}"),
            },
            4 => CredentialKey::NmxM {
                nmxm_id: format!("nmxm-{i}"),
            },
            5 => CredentialKey::NicLockdownIkm {
                credential_type: NicLockdownIkm::SiteWide { version: i as u32 },
            },
            _ => CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: mac(i),
            },
        };
        secrets.push((key, cred(&format!("user-{i}"), &format!("pass-{i}"))));
        i = i.wrapping_add(1);
    }

    secrets
}

/// Verifies the full vault-to-postgres import flow:
/// 1. Populate Vault with secrets.
/// 2. Import all secrets into Postgres.
/// 3. Verify all secrets are readable from Postgres.
/// 4. Re-import with MissingOnly and verify it's a noop.
/// 5. Verify the /_vault_import marker exists.
#[tokio::test]
async fn vault_to_postgres_import() -> eyre::Result<()> {
    // Skip if DATABASE_URL is not set (no Postgres available).
    let db_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("DATABASE_URL not set, skipping vault_to_postgres_import test");
            return Ok(());
        }
    };

    // Skip if vault binary is not in PATH.
    if std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter_map(|dir| {
            let candidate = dir.join("vault");
            candidate.is_file().then_some(candidate)
        })
        .next()
        .is_none()
    {
        eprintln!("vault binary not found in PATH, skipping vault_to_postgres_import test");
        return Ok(());
    }

    // --- Set up test database ---
    // Derive the test DB's connect options from the parsed admin options
    // rather than string-concatenating onto DATABASE_URL -- concatenation
    // breaks if the URL carries its own database path or query parameters.
    let admin_opts = PgConnectOptions::from_str(&db_url)?;
    let test_db_name = format!("secrets_import_test_{}", std::process::id());
    let admin_pool = PgPool::connect_with(admin_opts.clone()).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {test_db_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let test_opts = admin_opts.database(&test_db_name);

    // Everything from connecting onward runs in a fallible block whose
    // result is captured, so the database is dropped whether the import
    // succeeds, an assertion fails, or migrations error -- no leaked
    // temporary databases.
    let result = async {
        let test_pool = PgPool::connect_with(test_opts).await?;
        db::migrations::migrate(&test_pool).await?;
        let outcome = exercise_import(&test_pool).await;
        test_pool.close().await;
        outcome
    }
    .await;

    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {test_db_name}")))
        .execute(&admin_pool)
        .await?;

    result
}

/// The import flow itself: populate Vault, import into Postgres, verify
/// round-trips, re-import as a noop, and check the completion marker.
async fn exercise_import(test_pool: &PgPool) -> eyre::Result<()> {
    // --- Start Vault ---
    let vault = api_test_helper::vault::start().await?;
    let vault_config = VaultConfig {
        address: Some(format!("https://{}", vault.addr)),
        kv_mount_location: Some("secret".to_string()),
        pki_mount_location: Some("forgeca".to_string()),
        pki_role_name: Some("forge-cluster".to_string()),
        token: Some(vault.token.clone()),
        vault_cacert: Some(vault.ca_cert.clone()),
        ..Default::default()
    };
    let vault_client = create_vault_client(&vault_config)?;

    // --- Populate Vault with secrets ---
    let secrets = generate_test_secrets(100);
    for (key, cred) in &secrets {
        vault_client.set_credentials(key, cred).await?;
    }
    eprintln!("Populated {} secrets in Vault", secrets.len());

    // --- List all secrets from Vault ---
    let vault_secrets = vault_client.get_secrets().await?;
    assert_eq!(
        vault_secrets.len(),
        secrets.len(),
        "should list all populated secrets"
    );
    eprintln!("Listed {} secrets from Vault", vault_secrets.len());

    // --- Import into Postgres ---
    let encryption_key = make_test_key(42);
    let (routing, kms) = make_routing_and_kms(encryption_key);
    let mut work_lock_tasks = JoinSet::new();
    let work_lock_manager =
        db::work_lock_manager::start(&mut work_lock_tasks, test_pool.clone(), Default::default())
            .await?;
    let work_lock = work_lock_manager
        .try_acquire_lock("secrets-import-integration-test".to_string())
        .await?;

    let result = carbide_api_core::secrets::import_vault_secrets(
        test_pool,
        &work_lock,
        &routing,
        kms.as_ref(),
        &vault_secrets,
        carbide_api_core::secrets::ImportApproach::All,
    )
    .await?;

    assert_eq!(
        result,
        carbide_api_core::secrets::ImportResult::Completed {
            imported: secrets.len() as u64,
            skipped: 0,
        }
    );
    eprintln!("Imported {} secrets into Postgres", secrets.len());

    // --- Verify all secrets are readable from Postgres ---
    let pg_mgr = carbide_api_core::secrets::PostgresCredentialManager::new(
        test_pool.clone(),
        routing.clone(),
        kms.clone(),
    );
    for (key, expected_cred) in &secrets {
        let actual = pg_mgr.get_credentials(key).await?;
        assert_eq!(
            actual.as_ref(),
            Some(expected_cred),
            "secret at path {:?} should match",
            key.to_key_str()
        );
    }
    eprintln!("All {} secrets verified in Postgres", secrets.len());

    // --- The permanent marker makes any later import a no-op ---
    let result2 = carbide_api_core::secrets::import_vault_secrets(
        test_pool,
        &work_lock,
        &routing,
        kms.as_ref(),
        &vault_secrets,
        carbide_api_core::secrets::ImportApproach::MissingOnly,
    )
    .await?;

    assert_eq!(
        result2,
        carbide_api_core::secrets::ImportResult::AlreadyComplete
    );
    eprintln!("Permanent marker prevented a second import");

    // --- Verify marker ---
    assert!(
        carbide_api_core::secrets::is_vault_import_complete(test_pool).await?,
        "import marker should be set"
    );
    eprintln!("Import marker verified");

    work_lock.release().await?;
    drop(work_lock_manager);
    tokio::time::timeout(Duration::from_secs(3), work_lock_tasks.join_all())
        .await
        .expect("WorkLockManager did not shut down in a timely manner");

    Ok(())
}
