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
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::{env, path};

use carbide_secrets::credentials::{
    CredentialKey, CredentialType, CredentialWriter, Credentials, NicLockdownIkm,
};
use carbide_secrets::{CredentialConfig, VaultConfig, create_credential_manager};
use carbide_utils::HostPortPair;
use eyre::Report;
use metrics_endpoint::MetricsSetup;
use sqlx::migrate::MigrateDatabase;
use sqlx::{Pool, Postgres};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::api_server::StartArgs;
use crate::vault::Vault;
use crate::{api_server, vault};

lazy_static::lazy_static! {
    pub static ref REPO_ROOT: PathBuf = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    pub static ref LOCALHOST_CERTS: CertPaths = {
        let certs = REPO_ROOT.join("dev/certs/localhost");

        CertPaths {
            ca_cert: certs.join("ca.crt").canonicalize().unwrap(),
            server_cert: certs.join("localhost.crt").canonicalize().unwrap(),
            server_key: certs.join("localhost.key").canonicalize().unwrap(),
            client_cert: certs.join("client.crt").canonicalize().unwrap(),
            client_key: certs.join("client.key").canonicalize().unwrap(),
        }
    };
}

#[derive(Debug, Clone)]
pub struct IntegrationTestEnvironment {
    /// API client addresses. Each slot starts at `127.0.0.1:0`; [`start_api_server`] replaces it
    /// with the runtime-selected port before returning.
    pub carbide_api_addrs: Vec<SocketAddr>,
    pub root_dir: PathBuf,
    /// Metrics listener addresses. Each slot starts at `127.0.0.1:0`;
    /// [`start_api_server`] replaces it with the bound address before returning.
    pub carbide_metrics_addrs: Vec<SocketAddr>,
    pub db_url: String,
    pub db_pool: Pool<Postgres>,
    pub metrics: MetricsSetup,
    pub credential_config: CredentialConfig,
    pub _vault_handle: Arc<Vault>,
}

pub struct CertPaths {
    pub ca_cert: PathBuf,
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
}

impl IntegrationTestEnvironment {
    pub async fn try_from_environment(
        api_server_count: u8,
        database_name: &str,
    ) -> eyre::Result<Option<IntegrationTestEnvironment>> {
        let Ok(repo_root) = env::var("REPO_ROOT").or_else(|_| env::var("CONTAINER_REPO_ROOT"))
        else {
            eprintln!(
                "Either REPO_ROOT or CONTAINER_REPO_ROOT need to be set to run this test. Skipping."
            );
            return Ok(None);
        };
        let root_dir = PathBuf::from(repo_root.clone());

        let unbound_address = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let carbide_api_addrs = vec![unbound_address; usize::from(api_server_count)];
        let carbide_metrics_addrs = vec![unbound_address; usize::from(api_server_count)];

        // vault picks its own free port (retrying past races) and reports it
        // back on the handle, so we don't reserve one here.
        let vault = vault::start().await?;

        let credential_config = CredentialConfig {
            vault: VaultConfig {
                address: Some(format!("https://{}", vault.addr)),
                kv_mount_location: Some("secret".to_string()),
                pki_mount_location: Some("forgeca".to_string()),
                pki_role_name: Some("forge-cluster".to_string()),
                token: Some(vault.token.clone()),
                vault_cacert: Some(vault.ca_cert.clone()),
                ..Default::default()
            },
            ..Default::default()
        };

        // We have to do [sqlx::test] 's work manually here so that we can use a multi-threaded executor
        let db_url = env::var("DATABASE_URL")? + "/" + database_name;
        drop_pg_database_with_retry_if_exists(&db_url).await?;
        sqlx::Postgres::create_database(&db_url).await?;
        let db_pool = sqlx::Pool::<sqlx::postgres::Postgres>::connect(&db_url).await?;
        Ok(Some(IntegrationTestEnvironment {
            carbide_api_addrs,
            root_dir,
            carbide_metrics_addrs,
            credential_config,
            db_url,
            db_pool,
            metrics: {
                let metrics =
                    metrics_endpoint::new_metrics_setup("carbide-api", "forge-system", true)?; // unique to each test
                // Counts are process-wide; registering here puts
                // carbide_log_events_total on the in-process API's /metrics
                // (and, via the catalogue regeneration, in core_metrics.md).
                carbide_instrument::log_events::register(&metrics.meter);
                metrics
            },
            _vault_handle: Arc::new(vault),
        }))
    }
}

async fn drop_pg_database_with_retry_if_exists(db_url: &str) -> eyre::Result<()> {
    if !sqlx::Postgres::database_exists(db_url).await? {
        return Ok(());
    }
    let Some(database) = db_url.split('/').next_back() else {
        panic!("Invalid DATABASE_URL: {db_url}");
    };

    for attempt in 0..10 {
        match sqlx::Postgres::drop_database(db_url).await {
            Ok(()) => break,
            Err(e) => {
                eprintln!(
                    "Could not drop test database at {db_url} (will terminate all connections and retry {} more times): {e}",
                    10 - attempt
                )
            }
        }
        let db_pool = sqlx::Pool::<sqlx::postgres::Postgres>::connect(db_url).await?;
        let mut txn = db_pool.begin().await?;
        sqlx::query(
            r#"
        SELECT pg_terminate_backend(pg_stat_activity.pid)
        FROM pg_stat_activity
        WHERE datname = $1
        AND pid <> pg_backend_pid()"#,
        )
        .bind(database.to_string())
        .execute(&mut *txn)
        .await?;
        txn.commit().await?;
        db_pool.close().await;
        sleep(Duration::from_secs(5)).await;
    }
    Ok(())
}

pub struct TestApiServerArgs {
    pub bmc_proxy: Option<HostPortPair>,
    pub firmware_directory: PathBuf,
    pub addr_index: usize,
    pub put_dev_bin_in_path: bool,
    pub insecure_discovery: bool,
}

pub async fn start_api_server(
    test_env: &mut IntegrationTestEnvironment,
    TestApiServerArgs {
        bmc_proxy,
        firmware_directory,
        addr_index,
        put_dev_bin_in_path,
        insecure_discovery,
    }: TestApiServerArgs,
    cancel_token: CancellationToken,
) -> eyre::Result<ApiServerHandle> {
    let api_address = *test_env
        .carbide_api_addrs
        .get(addr_index)
        .ok_or_else(|| eyre::eyre!("API address index {addr_index} is out of range"))?;
    let metrics_address = *test_env
        .carbide_metrics_addrs
        .get(addr_index)
        .ok_or_else(|| eyre::eyre!("metrics address index {addr_index} is out of range"))?;
    let db_pool = test_env.db_pool.clone();
    let db_url = test_env.db_url.clone();
    let root_dir = test_env.root_dir.clone();
    let credential_config = test_env.credential_config.clone();

    // SAFETY: Initial lint enablement: these test settings are installed before the API
    // server task is spawned, but callers already run in multi-threaded test processes.
    // Unix process-wide exclusion from environment readers is not proven; this needs
    // owner review.
    unsafe {
        env::set_var("DISABLE_TLS_ENFORCEMENT", "true");
        env::set_var("IGNORE_MGMT_VRF", "true");
        // There is unfortunately no support for certificates in the vault dev server, so we have to disable this in code.
        env::set_var("UNSUPPORTED_CERTIFICATE_PROVIDER", "true");
        env::set_var("NO_DPU_CONTAINERS", "true");
        env::set_var("NO_DPU_ARMOS_NETWORK", "true");

        // Put our fake `crictl` on front of path so that forge-dpu-agent's HBN health checks succeed
        if put_dev_bin_in_path {
            let dev_bin = root_dir.join("dev/bin");
            if let Some(path) = env::var_os("PATH") {
                let mut paths = env::split_paths(&path).collect::<Vec<_>>();
                paths.insert(0, dev_bin);
                let new_path = env::join_paths(paths)?;
                env::set_var("PATH", new_path);
            }
        }
    }

    // We should setup logging here but:
    // - try_init sets a global logger and can only be called once.
    // Error is: "a global default trace dispatcher has already been set".
    // carbide_host_support::init_logging() calls try_init, but so does carbide-api when it starts.
    // - Even if we could get around that (carbide_host_support::subscriber().set_default() should
    // set a thread-specific logger), tracing will attempt to initialize the `log` crate (via tracing-log)
    // which can also only be initialized once. What a mess.
    // Error is: "attempted to set a logger after the logging system was already initialized"

    // Dependencies: Postgres, Vault and a Redfish BMC
    db::migrations::migrate(&db_pool).await?;

    populate_initial_vault_secrets(&credential_config).await?;

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let join_handle = tokio::spawn({
        let root_dir = root_dir.clone();
        let cancel_token = cancel_token.clone();
        async move {
            api_server::start(StartArgs {
                addr: api_address,
                metrics_addr: metrics_address,
                root_dir,
                db_url,
                bmc_proxy,
                firmware_directory,
                cancel_token,
                ready_channel: ready_tx,
                credential_config,
                insecure_discovery,
            })
            .await
            .inspect_err(|e| {
                eprintln!("Failed to start API server: {e:#}");
            })
        }
    });

    let addresses = match ready_rx.await {
        Ok(addresses) => addresses,
        Err(_error) => match join_handle.await {
            Ok(Err(error)) => return Err(error),
            Ok(Ok(())) => eyre::bail!("API server exited before reporting readiness"),
            Err(error) => return Err(error.into()),
        },
    };

    // `api_server::start` binds `[::]`, but test clients authenticate `127.0.0.1`; use the
    // listener's selected port without replacing the client IP.
    let listen_address = SocketAddr::new(api_address.ip(), addresses.listen_address.port());
    let Some(metrics_address) = addresses.metrics_address else {
        cancel_token.cancel();
        match join_handle.await {
            Ok(Err(error)) => return Err(error),
            Ok(Ok(())) => eyre::bail!("API server reported readiness without a metrics listener"),
            Err(error) => return Err(error.into()),
        }
    };

    test_env.carbide_api_addrs[addr_index] = listen_address;
    test_env.carbide_metrics_addrs[addr_index] = metrics_address;

    Ok(ApiServerHandle { join_handle })
}

/// When dropped, this will invalidate the API server.
pub struct ApiServerHandle {
    join_handle: JoinHandle<eyre::Result<()>>,
}

impl ApiServerHandle {
    pub async fn wait(self) -> eyre::Result<()> {
        self.join_handle.await.expect("task panicked")
    }
}

pub async fn populate_initial_vault_secrets(
    credential_config: &CredentialConfig,
) -> Result<(), Report> {
    let credential_manager = create_credential_manager(credential_config).await?;
    credential_manager
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: carbide_secrets::credentials::BmcCredentialType::SiteWideRoot,
            },
            &Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            },
        )
        .await?;

    credential_manager
        .set_credentials(
            &CredentialKey::DpuUefi {
                credential_type: CredentialType::SiteDefault,
            },
            &Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            },
        )
        .await?;

    credential_manager
        .set_credentials(
            &CredentialKey::DpuUefi {
                credential_type: CredentialType::DpuHardwareDefault {
                    model: bmc_vendor::DpuModel::Unknown,
                },
            },
            &Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            },
        )
        .await?;

    credential_manager
        .set_credentials(
            &CredentialKey::HostUefi {
                credential_type: CredentialType::SiteDefault,
            },
            &Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            },
        )
        .await?;

    credential_manager
        .set_credentials(
            &CredentialKey::NicLockdownIkm {
                credential_type: NicLockdownIkm::SiteWide { version: 0 },
            },
            &Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "test-lockdown-ikm".to_string(),
            },
        )
        .await?;
    Ok(())
}

pub fn find_prerequisites() -> eyre::Result<HashMap<String, PathBuf>> {
    let mut bins = HashMap::with_capacity(2);
    let paths: Vec<path::PathBuf> = env::split_paths(&env::var_os("PATH").unwrap()).collect();
    bins.insert("vault", find_first_in("vault", &paths));
    bins.insert("curl", find_first_in("curl", &paths));

    let mut full_paths = HashMap::with_capacity(bins.len());
    for (k, v) in bins.drain() {
        match v {
            Some(full_path) => {
                full_paths.insert(k.to_string(), full_path);
            }
            None => {
                eyre::bail!("missing prerequisite binary: {k}");
            }
        }
    }

    Ok(full_paths)
}

// Look for a binary in the given paths, return full path or None if not found
fn find_first_in(binary: &str, paths: &[path::PathBuf]) -> Option<path::PathBuf> {
    for path in paths {
        let candidate = path.join(binary);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
