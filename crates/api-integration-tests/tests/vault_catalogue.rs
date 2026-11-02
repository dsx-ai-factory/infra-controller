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

use std::net::Ipv6Addr;
use std::time::Duration;

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialPrefix, CredentialReader, CredentialWriter,
    Credentials, MqttCredentialType,
};
use carbide_secrets::{ForgeVaultClient, VaultConfig, create_vault_client};
use eyre::WrapErr;
use mac_address::MacAddress;
use serial_test::serial;

fn cred(user: &str, pass: &str) -> Credentials {
    Credentials::UsernamePassword {
        username: user.to_string(),
        password: pass.to_string(),
    }
}

#[tokio::test]
async fn credentials_round_trip_over_ipv6_tls() -> eyre::Result<()> {
    if !std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join("vault").is_file())
    {
        eprintln!("Skipping IPv6 Vault test: vault binary not found in PATH");
        return Ok(());
    }

    let mut vault = api_test_helper::vault::start_on(Ipv6Addr::LOCALHOST.into()).await?;
    let key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::SiteWideRoot,
    };
    let expected = cred("ipv6-bmc-root", "ipv6-bmc-password");
    let exchange = tokio::time::timeout(Duration::from_secs(10), async {
        let config = VaultConfig {
            address: Some(format!("https://{}", vault.addr)),
            kv_mount_location: Some("secret".to_string()),
            pki_mount_location: Some("forgeca".to_string()),
            pki_role_name: Some("forge-cluster".to_string()),
            token: Some(vault.token.clone()),
            vault_cacert: Some(vault.ca_cert.clone()),
            ..Default::default()
        };
        let client = create_vault_client(&config)?;
        client.set_credentials(&key, &expected).await?;
        Ok::<_, eyre::Report>(client.get_credentials(&key).await?)
    })
    .await;

    tokio::time::timeout(Duration::from_secs(5), vault.process.kill())
        .await
        .wrap_err("timed out stopping IPv6 vault")?
        .wrap_err("stopping IPv6 vault")?;
    let actual = exchange.wrap_err("timed out writing and reading IPv6 vault credentials")??;
    assert_eq!(actual, Some(expected));
    Ok(())
}

/// Sets up a Vault dev server with some test
/// secrets. Returns the vault handle (must be held
/// alive), the client, and the populated secrets.
async fn setup_vault_with_secrets() -> Option<(
    api_test_helper::vault::Vault,
    std::sync::Arc<ForgeVaultClient>,
    Vec<(CredentialKey, Credentials)>,
)> {
    // Skip if vault not in PATH.
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter_map(|dir| {
            let c = dir.join("vault");
            c.is_file().then_some(c)
        })
        .next()?;

    let vault = api_test_helper::vault::start().await.expect("start vault");

    let config = VaultConfig {
        address: Some(format!("https://{}", vault.addr)),
        kv_mount_location: Some("secret".to_string()),
        pki_mount_location: Some("forgeca".to_string()),
        pki_role_name: Some("forge-cluster".to_string()),
        token: Some(vault.token.clone()),
        vault_cacert: Some(vault.ca_cert.clone()),
        ..Default::default()
    };
    let client = create_vault_client(&config).expect("create vault client");

    // Populate a mix of secrets across prefixes.
    let secrets = vec![
        (
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRoot,
            },
            cred("bmc-root", "bmc-pass"),
        ),
        (
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: MacAddress::new([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]),
                },
            },
            cred("bmc-specific", "bmc-pass-2"),
        ),
        (
            CredentialKey::UfmAuth {
                fabric: "fabric-1".to_string(),
            },
            cred("ufm-user", "ufm-pass"),
        ),
        (
            CredentialKey::MqttAuth {
                credential_type: MqttCredentialType::Dpa,
            },
            cred("mqtt-user", "mqtt-pass"),
        ),
    ];

    for (key, c) in &secrets {
        client.set_credentials(key, c).await.expect("set");
    }

    Some((vault, client, secrets))
}

// Verifies that list_secrets returns all paths.
#[tokio::test]
#[serial]
async fn list_secrets_returns_all_paths() {
    let (_vault, client, secrets) = match setup_vault_with_secrets().await {
        Some(v) => v,
        None => {
            eprintln!("vault not available, skipping");
            return;
        }
    };

    let paths = client.list_secrets().await.expect("list");
    assert!(
        paths.len() >= secrets.len(),
        "should list at least all populated secrets"
    );

    // Every populated secret should appear.
    for (key, _) in &secrets {
        let path = key.to_key_str();
        assert!(
            paths.iter().any(|p| p == path.as_ref()),
            "path {path:?} should be in list"
        );
    }
}

// Verifies that list_secrets_for_prefix scopes to
// the correct prefix.
#[tokio::test]
#[serial]
async fn list_secrets_for_prefix_scopes_correctly() {
    let (_vault, client, _) = match setup_vault_with_secrets().await {
        Some(v) => v,
        None => {
            eprintln!("vault not available, skipping");
            return;
        }
    };

    // BMC prefix should return 2 secrets.
    let bmc_paths = client
        .list_secrets_for_prefix(&CredentialPrefix::BmcCredentials)
        .await
        .expect("list bmc");
    assert_eq!(bmc_paths.len(), 2, "should have 2 BMC secrets");

    // UFM prefix should return 1.
    let ufm_paths = client
        .list_secrets_for_prefix(&CredentialPrefix::UfmAuth)
        .await
        .expect("list ufm");
    assert_eq!(ufm_paths.len(), 1, "should have 1 UFM secret");

    // MQTT prefix should return 1.
    let mqtt_paths = client
        .list_secrets_for_prefix(&CredentialPrefix::MqttAuth)
        .await
        .expect("list mqtt");
    assert_eq!(mqtt_paths.len(), 1, "should have 1 MQTT secret");
}

// Verifies that list_secrets_for_prefix returns
// empty for a prefix with no secrets.
#[tokio::test]
#[serial]
async fn list_secrets_for_prefix_empty_returns_empty() {
    let (_vault, client, _) = match setup_vault_with_secrets().await {
        Some(v) => v,
        None => {
            eprintln!("vault not available, skipping");
            return;
        }
    };

    // NmxM has no secrets populated.
    let paths = client
        .list_secrets_for_prefix(&CredentialPrefix::NmxM)
        .await
        .expect("list nmxm");
    assert!(paths.is_empty(), "should have no NmxM secrets");
}

// Verifies that get_secrets reads all values.
#[tokio::test]
#[serial]
async fn get_secrets_reads_all_values() {
    let (_vault, client, secrets) = match setup_vault_with_secrets().await {
        Some(v) => v,
        None => {
            eprintln!("vault not available, skipping");
            return;
        }
    };

    let got = client.get_secrets().await.expect("get");
    assert!(got.len() >= secrets.len());

    // Verify each credential matches.
    for (key, expected) in &secrets {
        let path = key.to_key_str();
        let (_, actual) = got
            .iter()
            .find(|(p, _)| p == path.as_ref())
            .unwrap_or_else(|| panic!("secret {path:?} not found"));
        assert_eq!(actual, expected, "credentials for {path:?} should match");
    }
}

// Verifies that get_secrets_for_prefix reads only
// secrets under the given prefix.
#[tokio::test]
#[serial]
async fn get_secrets_for_prefix_reads_scoped() {
    let (_vault, client, _) = match setup_vault_with_secrets().await {
        Some(v) => v,
        None => {
            eprintln!("vault not available, skipping");
            return;
        }
    };

    let bmc = client
        .get_secrets_for_prefix(&CredentialPrefix::BmcCredentials)
        .await
        .expect("get bmc");
    assert_eq!(bmc.len(), 2);

    // Verify all returned paths start with the
    // BMC prefix.
    for (path, _) in &bmc {
        assert!(
            path.starts_with(CredentialPrefix::BmcCredentials.as_str()),
            "path {path:?} should start with BMC prefix"
        );
    }

    // Verify credentials are correct.
    let site_root = bmc
        .iter()
        .find(|(p, _)| p.ends_with("site/root"))
        .expect("site root");
    assert_eq!(site_root.1, cred("bmc-root", "bmc-pass"));
}
