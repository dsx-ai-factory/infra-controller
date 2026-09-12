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

use std::collections::BTreeMap;
use std::sync::Arc;

use carbide_api_core::bootstrap::acquire_vault_import_work_lock;
use carbide_api_core::cfg::file::{
    CarbideConfig, CredentialBackend, ImportSource, ProviderConfig, SecretsConfig,
    UfmCredentialSource,
};
use carbide_api_core::secrets::{PostgresCredentialManager, SecretRouting, SecretsContext};
use carbide_kms_provider::{
    DEFAULT_TRANSIT_MOUNT, IntegratedKmsProvider, KmsBackend, MultiKmsProvider, TransitKmsProvider,
};
use carbide_secrets::certificates::CertificateProvider;
use carbide_secrets::chained_reader::{NonUfmCredentialReader, UfmBackendCredentialBlocker};
use carbide_secrets::credentials::{
    CredentialManager, CredentialPrefix, CredentialReader, CredentialWriter,
    UfmCredentialMutationBlocker,
};
use carbide_secrets::{
    CredentialConfig, ForgeVaultClient, MemoryCredentialStore, SecretsError, SpiffeIdentity,
    VaultConfig, create_certificate_provider, create_credential_manager_from, create_vault_client,
};
use db::work_lock_manager::{self, WorkLockManagerHandle};
use eyre::WrapErr;
use sqlx::postgres::PgSslMode;
use sqlx::{ConnectOptions, PgPool};
use sqlx_query_tracing::SQLX_STATEMENTS_LOG_LEVEL;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_log::AsLog as _;

// Currently WorkLockManager needs a dedicated, reserved connection so that we can release locks
// properly even if the main pool is full.
static RESERVED_CONNECTION_COUNT: usize = 1;

pub(crate) struct RuntimeResources {
    pub credential_manager: Arc<dyn CredentialManager>,
    pub certificate_provider: Arc<dyn CertificateProvider>,
    pub db_pool: PgPool,
    pub work_lock_manager_handle: WorkLockManagerHandle,
    pub secrets_context: Option<SecretsContext>,
}

type CredentialRuntimeParts = (
    Arc<dyn CredentialWriter>,
    Vec<Box<dyn CredentialReader>>,
    Option<SecretsContext>,
);

pub(crate) async fn setup_resources(
    carbide_config: &CarbideConfig,
    credential_config: &CredentialConfig,
    join_set: &mut JoinSet<()>,
    cancel_token: &CancellationToken,
) -> eyre::Result<RuntimeResources> {
    let vault_config = vault_config_for_site(&credential_config.vault, carbide_config);

    // One vault client serves every credential vault role below.
    let vault_client = create_vault_client(&vault_config)?;

    // Certificate vending is selected independently of the credential store.
    // SharedVault (the default) reuses `vault_client` (no second client or token
    // lease); a dedicated cert Vault decouples PKI issuance from credentials and
    // is fully explicit, never inheriting the credential Vault's env config. The
    // SPIFFE identity comes from the site-resolved credential Vault config so all
    // backends mint under the same identity namespace.
    let cert_config = carbide_config.certificates.to_certificate_config()?;
    let certificate_provider = create_certificate_provider(
        &cert_config,
        &vault_client,
        SpiffeIdentity {
            trust_domain: vault_config.spiffe_trust_domain(),
            machine_base_path: vault_config.spiffe_machine_base_path(),
        },
    )?;

    let db_pool = connect_postgres(carbide_config).await?;
    let work_lock_manager_handle = work_lock_manager::start(
        join_set,
        db_pool.clone(),
        work_lock_manager::KeepaliveConfig::default(),
    )
    .await?;

    // Build the local-override readers. The config-file credential source, if
    // set, overrides the legacy environment-selected file source; env remains
    // opt-in through its legacy environment configuration. The backends
    // (postgres, vault) and the writer are chosen below.
    let local_overrides = local_credential_readers(carbide_config, credential_config).await?;

    // With a [secrets] section, the credential chain and write target come from
    // `backends`/`writer` -- generally defaulting to env -> file -> vault
    // writing to vault. `credentials.ufm_source` can retain that read order or
    // select one authoritative UFM source. The one-time vault import is
    // independent: it runs iff `import_from` is set. Without the section, the
    // store comes from CARBIDE_CREDENTIAL_STORE: vault (the default), or an
    // in-memory store for development and testing.
    let (writer, chain, secrets_context): CredentialRuntimeParts = if let Some(secrets_config) =
        &carbide_config.secrets
    {
        // Reject a nonsensical backends list before anything with side effects
        // runs (KMS task setup, the one-time vault import): a config error
        // should fail the boot cleanly, not after a partial, hard-to-undo
        // import that has already written the completion marker.
        carbide_api_core::secrets::validate_backends(&secrets_config.backends)?;
        let routing = SecretRouting::from_config(&secrets_config.routing)
            .map_err(eyre::Report::new)
            .wrap_err("secrets routing configuration")?;
        let kms = build_kms_backend(
            secrets_config,
            &vault_config,
            &routing,
            join_set,
            cancel_token,
        )?;
        let pg_manager = Arc::new(PostgresCredentialManager::new(
            db_pool.clone(),
            routing.clone(),
            kms.clone(),
        ));
        tracing::info!(
            active_provider = %secrets_config.kms.active,
            backends = ?secrets_config.backends,
            writer = ?secrets_config.writer,
            "Postgres secrets backend configured"
        );

        // New writes all go to `writer`, but reads take the first backend in
        // `backends` that holds the path -- first-match-wins, evaluated per path.
        // So unless `writer` is the highest-priority backend, a write can be
        // shadowed: if a higher-priority backend also holds that path, reads keep
        // returning its value and never reach the writer's. E.g. with
        // backends = [vault, postgres] and writer = postgres, a path that exists
        // in both reads vault's value until vault's copy of that path is
        // removed (a read-after-write gap). The same gap exists when `writer`
        // is not in `backends` at all. Both reduce to "writer isn't the top
        // backend." We allow it -- a deliberate shadow-write is a valid, if
        // advanced, setup -- but warn so an accidental one is visible.
        if secrets_config.backends.first() != Some(&secrets_config.writer) {
            tracing::warn!(
                writer = ?secrets_config.writer,
                backends = ?secrets_config.backends,
                "secrets writer's backend is not the highest-priority backend: a write to a path a \
                 higher-priority backend also holds is shadowed on read until that copy is removed \
                 (read-after-write gap)"
            );
        }

        // A one-time bulk import from vault, only when the operator asks for
        // one. Independent of backends/writer.
        if secrets_config.import_from == Some(ImportSource::Vault) {
            import_vault_secrets_once(
                &db_pool,
                &work_lock_manager_handle,
                VaultImportOptions {
                    secrets: secrets_config,
                    exclude_ufm: carbide_config
                        .credentials
                        .uses_authoritative_local_ufm_credentials(),
                },
                &routing,
                kms.as_ref(),
                &vault_client,
                cancel_token,
            )
            .await?;
        }

        // Read order: the always-first local overrides, then the configured
        // backends in the operator's chosen order (first match wins). The write
        // target is the single backend `writer` names. (`backends` was
        // validated at the top of this branch, before any side effects.)
        let backend_readers =
            secrets_config
                .backends
                .iter()
                .map(|backend| -> Box<dyn CredentialReader> {
                    match backend {
                        CredentialBackend::Postgres => Box::new(pg_manager.clone()),
                        CredentialBackend::Vault => Box::new(vault_client.clone()),
                    }
                });
        let chain = local_overrides.into_iter().chain(backend_readers).collect();
        let writer: Arc<dyn CredentialWriter> = match secrets_config.writer {
            CredentialBackend::Vault => vault_client.clone(),
            CredentialBackend::Postgres => pg_manager,
        };
        (writer, chain, Some(SecretsContext { routing, kms }))
    } else {
        let store: Arc<dyn CredentialManager> = match std::env::var("CARBIDE_CREDENTIAL_STORE")
            .as_deref()
            .unwrap_or("vault")
        {
            "vault" => vault_client,
            "memory" => Arc::new(MemoryCredentialStore::default()),
            other => {
                return Err(eyre::eyre!(
                    "invalid CARBIDE_CREDENTIAL_STORE value {other:?}: expected \"vault\" or \"memory\""
                ));
            }
        };
        // env -> file -> the configured store; nothing from [secrets] applies.
        // The local UFM blocker, when configured, stops before the store.
        let chain = local_overrides
            .into_iter()
            .chain(std::iter::once(
                Box::new(store.clone()) as Box<dyn CredentialReader>
            ))
            .collect();
        (store, chain, None)
    };
    // Apply the UFM mutation policy once, after selecting either persistent
    // writer path, so the two setup branches cannot drift apart.
    let writer = credential_writer_with_ufm_policy(carbide_config, writer);
    let credential_manager = create_credential_manager_from(writer, chain);

    Ok(RuntimeResources {
        credential_manager,
        certificate_provider,
        db_pool,
        work_lock_manager_handle,
        secrets_context,
    })
}

/// Vault machine PKI URI SANs must match `[auth.trust]` when site auth config is present.
fn vault_config_for_site(vault: &VaultConfig, carbide_config: &CarbideConfig) -> VaultConfig {
    let mut config = vault.clone();
    if let Some(trust) = carbide_config
        .auth
        .as_ref()
        .and_then(|auth| auth.trust.as_ref())
    {
        config.spiffe_trust_domain = Some(trust.spiffe_trust_domain.clone());
        config.spiffe_machine_base_path = Some(trust.spiffe_machine_base_path.clone());
    }
    config
}

/// Configure and create a postgres connection pool
///
/// This connects to the database to verify settings
async fn connect_postgres(config: &CarbideConfig) -> eyre::Result<PgPool> {
    validate_database_pool_durations([
        (
            "database_pool_acquire_timeout",
            config.database_pool_acquire_timeout,
        ),
        (
            "database_pool_idle_timeout",
            config.database_pool_idle_timeout,
        ),
        (
            "database_pool_max_lifetime",
            config.database_pool_max_lifetime,
        ),
    ])?;

    // We need logs to be enabled at least at `INFO` level. Otherwise
    // our global logging filter would reject the logs before they get injected
    // into the `SqlxQueryTracing` layer.
    let mut options = config
        .database_url
        .parse::<sqlx::postgres::PgConnectOptions>()?
        .log_statements(SQLX_STATEMENTS_LOG_LEVEL.as_log().to_level_filter());
    // The integration test opts out of TLS enforcement.
    if let Some(tls_config) = &config.tls
        && std::env::var("DISABLE_TLS_ENFORCEMENT").is_err()
    {
        tracing::info!("using TLS for postgres connection.");
        options = options
            // TODO: move this to VerifyFull once it actually works.
            .ssl_mode(PgSslMode::Require)
            .ssl_root_cert(&tls_config.root_cafile_path);
    }

    let max_connections = config
        .max_database_connections
        .saturating_sub(RESERVED_CONNECTION_COUNT as u32);
    if max_connections == 0 {
        eyre::bail!(
            "config.max_database_connections is too small to accommodate reserved connections: must be greater than {RESERVED_CONNECTION_COUNT}"
        );
    }

    let pool_options = sqlx::pool::PoolOptions::new()
        .max_connections(max_connections)
        // Lifecycle settings are operator-configurable; each `database_pool_*`
        // config field documents what it bounds. The defaults are sqlx's own,
        // so exposing them changes no behavior -- tuning belongs to the site.
        .acquire_timeout(config.database_pool_acquire_timeout)
        .idle_timeout(Some(config.database_pool_idle_timeout))
        .max_lifetime(Some(config.database_pool_max_lifetime));

    connect_with_retry(pool_options, options, config.database_startup_retry_timeout).await
}

/// Establish the initial database connection, retrying with bounded
/// exponential backoff for up to `retry_timeout` instead of failing on the
/// first error. A transient outage (failover, rolling upgrade, DNS blip)
/// that clears within the window no longer takes the whole process down;
/// a genuinely unreachable database still fails once the window elapses.
///
/// Each attempt itself is capped by whatever time remains in the window
/// (independent of `database_pool_acquire_timeout`), so a single slow or
/// hanging attempt -- e.g. a DNS lookup that never returns -- can't run
/// the total past `retry_timeout`. Once the window is exhausted, no further
/// attempt is even started -- the last error is returned as-is, so a
/// connection that happens to become ready in that same instant doesn't
/// count as succeeding within a window that's already closed. `retry_timeout`
/// of zero attempts the connection exactly once, unbounded by this function
/// (matching the old behavior, subject only to the pool's own
/// `acquire_timeout`).
async fn connect_with_retry(
    pool_options: sqlx::pool::PoolOptions<sqlx::Postgres>,
    connect_options: sqlx::postgres::PgConnectOptions,
    retry_timeout: std::time::Duration,
) -> eyre::Result<PgPool> {
    const INITIAL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
    const MAX_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

    // Runs one attempt, bounded to `remaining` (`None` for a zero
    // `retry_timeout`, meaning unbounded here).
    async fn try_connect(
        pool_options: &sqlx::pool::PoolOptions<sqlx::Postgres>,
        connect_options: &sqlx::postgres::PgConnectOptions,
        remaining: Option<std::time::Duration>,
    ) -> Result<PgPool, sqlx::Error> {
        let attempt = pool_options.clone().connect_with(connect_options.clone());
        match remaining {
            Some(remaining) => tokio::time::timeout(remaining, attempt)
                .await
                .unwrap_or(Err(sqlx::Error::PoolTimedOut)),
            None => attempt.await,
        }
    }

    let start = std::time::Instant::now();
    let mut retry_delay = INITIAL_RETRY_DELAY;
    let time_remaining = |elapsed: std::time::Duration| {
        (!retry_timeout.is_zero()).then(|| retry_timeout.saturating_sub(elapsed))
    };

    let mut error = match try_connect(
        &pool_options,
        &connect_options,
        time_remaining(start.elapsed()),
    )
    .await
    {
        Ok(pool) => return Ok(pool),
        Err(error) => error,
    };

    while !retry_timeout.is_zero() && start.elapsed() < retry_timeout {
        let elapsed = start.elapsed();
        tracing::warn!(
            %error,
            elapsed_secs = elapsed.as_secs_f64(),
            retry_in_secs = retry_delay.as_secs_f64(),
            "failed to connect to postgres at startup, retrying"
        );
        tokio::time::sleep(retry_delay.min(retry_timeout.saturating_sub(elapsed))).await;
        retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);

        // The sleep above may have used up the rest of the window. Only
        // attempt again if time actually remains -- otherwise stop without
        // touching `error`, so a window that closes mid-sleep still reports
        // the real error from the last actual attempt instead of a
        // synthetic "out of time" one that would obscure it.
        let Some(remaining) = time_remaining(start.elapsed()).filter(|r| !r.is_zero()) else {
            break;
        };
        error = match try_connect(&pool_options, &connect_options, Some(remaining)).await {
            Ok(pool) => return Ok(pool),
            Err(error) => error,
        };
    }

    let elapsed = start.elapsed();
    tracing::warn!(
        %error,
        elapsed_secs = elapsed.as_secs_f64(),
        "failed to connect to postgres at startup, giving up"
    );
    Err(error).wrap_err_with(|| {
        if retry_timeout.is_zero() {
            "failed to connect to postgres".to_string()
        } else {
            format!("failed to connect to postgres after retrying for {elapsed:.0?}")
        }
    })
}

fn validate_database_pool_durations(
    durations: [(&str, std::time::Duration); 3],
) -> eyre::Result<()> {
    for (name, value) in durations {
        if value.is_zero() {
            eyre::bail!("{name} must be greater than zero");
        }
    }
    Ok(())
}

async fn local_credential_readers(
    carbide_config: &CarbideConfig,
    credential_config: &CredentialConfig,
) -> eyre::Result<Vec<Box<dyn CredentialReader>>> {
    let env_reader: Option<Box<dyn CredentialReader>> = if credential_config.env.enabled() {
        Some(Box::new(
            carbide_secrets::local_credentials::EnvCredentials::new(credential_config.env.clone())?,
        ))
    } else {
        None
    };
    let file_config = file_credentials_config(carbide_config, credential_config);
    let file_reader: Option<Box<dyn CredentialReader>> = if file_config.enabled() {
        Some(Box::new(
            carbide_secrets::local_credentials::FileCredentialsWatcher::new(file_config).await?,
        ))
    } else {
        None
    };
    let mut local_readers: Vec<Box<dyn CredentialReader>> =
        [env_reader, file_reader].into_iter().flatten().collect();
    match carbide_config.credentials.ufm_source {
        UfmCredentialSource::LocalFirst => Ok(local_readers),
        UfmCredentialSource::Backend => {
            let local_chain = carbide_secrets::ChainedCredentialReader::from(local_readers);
            Ok(vec![Box::new(NonUfmCredentialReader::new(local_chain))])
        }
        UfmCredentialSource::Local => {
            validate_local_ufm_credentials(carbide_config, &local_readers).await?;
            tracing::info!(
                "local environment/file sources own UFM credentials; persistent backend access is \
                 disabled"
            );
            local_readers.push(Box::new(UfmBackendCredentialBlocker));
            Ok(local_readers)
        }
    }
}

async fn validate_local_ufm_credentials(
    carbide_config: &CarbideConfig,
    local_readers: &[Box<dyn CredentialReader>],
) -> eyre::Result<()> {
    if !carbide_config
        .ib_config
        .as_ref()
        .is_some_and(|config| config.enabled)
    {
        return Ok(());
    }
    for fabric in carbide_config.ib_fabrics.keys() {
        let key = carbide_secrets::credentials::CredentialKey::UfmAuth {
            fabric: fabric.clone(),
        };
        let mut found = false;
        for reader in local_readers {
            if reader.get_credentials(&key).await?.is_some() {
                found = true;
                break;
            }
        }
        if !found {
            return Err(SecretsError::UfmCredentialReadBlocked {
                fabric: fabric.clone(),
            }
            .into());
        }
    }
    Ok(())
}

fn file_credentials_config(
    carbide_config: &CarbideConfig,
    credential_config: &CredentialConfig,
) -> carbide_secrets::FileCredentialsConfig {
    carbide_config
        .credentials
        .file
        .as_ref()
        .map(|file| carbide_secrets::FileCredentialsConfig {
            enabled: Some(true),
            path: Some(file.path.clone()),
            poll_interval: Some(file.poll_interval),
        })
        .unwrap_or_else(|| credential_config.file.clone())
}

fn credential_writer_with_ufm_policy(
    carbide_config: &CarbideConfig,
    writer: Arc<dyn CredentialWriter>,
) -> Arc<dyn CredentialWriter> {
    if carbide_config
        .credentials
        .uses_authoritative_local_ufm_credentials()
    {
        Arc::new(UfmCredentialMutationBlocker::new(writer))
    } else {
        writer
    }
}

/// Build the KMS stack from the `[secrets.kms]` config: construct every
/// named provider, check the routed KEKs against them, and combine them so
/// the active provider wraps DEKs for new writes while any provider can
/// unwrap rows recorded with its kek_ids.
fn build_kms_backend(
    config: &SecretsConfig,
    vault_config: &VaultConfig,
    routing: &SecretRouting,
    join_set: &mut JoinSet<()>,
    cancel_token: &CancellationToken,
) -> eyre::Result<Arc<dyn KmsBackend>> {
    // BTreeMap so the provider list below has a stable order -- with
    // duplicate kek_ids rejected, order never decides which provider
    // unwraps, but stable beats arbitrary if that invariant ever slips.
    let mut built: BTreeMap<String, Arc<dyn KmsBackend>> = BTreeMap::new();
    for (name, provider_config) in &config.kms.providers {
        let provider: Arc<dyn KmsBackend> = match provider_config {
            ProviderConfig::Integrated { keys } => Arc::new(
                IntegratedKmsProvider::from_config(keys)
                    .map_err(eyre::Report::new)
                    .wrap_err_with(|| format!("KMS provider {name:?} key configuration"))?,
            ),
            ProviderConfig::Transit {
                keys,
                transit_mount,
            } => {
                // The same address, CA trust, and timeout ForgeVaultClient
                // connects with -- a bare vaultrs client only trusts public
                // roots and fails TLS against a site-CA-signed vault.
                let settings = carbide_secrets::create_raw_vault_client_settings(vault_config)
                    .wrap_err(
                        "building the transit KMS vault client (transit requires a static \
                         VAULT_TOKEN; the kubernetes service-account login flow is not \
                         supported for transit yet)",
                    )?;
                let client = Arc::new(
                    vaultrs::client::VaultClient::new(settings)
                        .map_err(|error| eyre::eyre!("vault client: {error}"))?,
                );
                let provider = TransitKmsProvider::new(
                    client,
                    transit_mount
                        .as_deref()
                        .unwrap_or(DEFAULT_TRANSIT_MOUNT)
                        .to_string(),
                    keys.clone(),
                );
                join_set
                    .build_task()
                    .name("transit_kms_token_renewal")
                    .spawn(provider.run_token_renewal(cancel_token.clone()))?;
                Arc::new(provider)
            }
        };
        tracing::info!(name = %name, "initialized KMS provider");
        built.insert(name.clone(), provider);
    }

    let active = built
        .get(&config.kms.active)
        .ok_or_else(|| {
            eyre::eyre!(
                "active KMS provider {:?} not found; configured providers: {:?}",
                config.kms.active,
                built.keys().collect::<Vec<_>>()
            )
        })?
        .clone();

    // Check the config against itself now, while a mismatch is a config
    // mistake. Found at runtime instead, a missing key is a write failure
    // on whichever credential first routes to it, and a duplicated key
    // makes unwraps depend on provider order.
    //
    // Every routed KEK must exist in the active provider, because all new
    // DEK wraps go through it. And no KEK may exist in two providers --
    // checked across every configured KEK, not just the routed ones,
    // because rows wrapped by a rotated-out KEK still unwrap through
    // whichever provider has it.
    for (prefix, kek_id) in routing.routes() {
        if !active.can_decrypt_kek(kek_id) {
            return Err(eyre::eyre!(
                "routing assigns {kek_id:?} (prefix {prefix:?}), but the active KMS provider {:?} does not have that key",
                config.kms.active
            ));
        }
    }

    let mut owners: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for (name, provider) in &built {
        // Dedup within a provider first: a transit key list can repeat a
        // name, and that is harmless, not "two providers".
        let mut kek_ids = provider.kek_ids();
        kek_ids.sort();
        kek_ids.dedup();
        for kek_id in kek_ids {
            owners.entry(kek_id).or_default().push(name);
        }
    }
    for (kek_id, owners) in owners {
        if owners.len() > 1 {
            return Err(eyre::eyre!(
                "kek_id {kek_id:?} exists in more than one KMS provider ({owners:?}); unwraps would be ambiguous"
            ));
        }
    }

    Ok(Arc::new(MultiKmsProvider::new(
        active,
        built.into_values().collect(),
    )))
}

/// Run the one-time vault import, skipping if the completion marker is
/// already written. The caller gates this on `import_from` (a fresh site
/// simply omits it), so by the time we are here an import is wanted.
///
/// The import either completes before this process serves traffic, or the
/// process does not start: enumeration is strict (any vault list or read
/// failure aborts the boot), and an empty enumeration aborts too, because
/// an empty vault on a site configured to import from it is far more
/// likely a vault problem than a truly empty vault. A genuinely fresh
/// site simply omits `import_from`. Keeping it strict gives a clean,
/// all-or-nothing bulk copy with no half-imported state to reason about.
///
/// This is orthogonal to the reader chain and writer: an import seeds
/// Postgres with eligible Vault secrets, but the read order and write target
/// stay exactly as `backends` / `writer` set them -- importing changes neither.
/// UFM paths are ineligible while local sources own UFM credentials so the
/// import cannot bypass that site-wide ownership boundary.
///
/// Rolling upgrades still need care once writes move to Postgres: a replica
/// running an older config can write rotated credentials to its own writer,
/// where they are stranded. Site-explorer credential rotation is the writer
/// to worry about; keep it disabled until the whole fleet runs a consistent
/// config.
///
/// During a rolling upgrade, the fenced transaction also takes the marker
/// advisory lock used by the old session-lock importer. Healthy old and new
/// importer sessions therefore serialize even though only the new replicas use
/// `WorkLockManager`. The old implementation cannot be fenced if its detached
/// lock session dies while its importer keeps running, so deploy this code to
/// every API replica before starting an import on a site without the marker.
///
/// Normal returns and errors wait for the manager to release the work lock; a
/// hard crash falls back to its lease expiry. Vault and KMS work is prepared
/// before the transaction atomically commits every secret plus the marker, so
/// an expired owner cannot write after a replacement takes over.
struct VaultImportOptions<'a> {
    secrets: &'a SecretsConfig,
    exclude_ufm: bool,
}

async fn import_vault_secrets_once(
    db_pool: &PgPool,
    work_lock_manager: &WorkLockManagerHandle,
    options: VaultImportOptions<'_>,
    routing: &SecretRouting,
    kms: &dyn KmsBackend,
    vault_client: &ForgeVaultClient,
    cancel_token: &CancellationToken,
) -> eyre::Result<()> {
    if is_import_complete(db_pool).await? {
        tracing::info!("Vault import already completed");
        return Ok(());
    }

    // Several replicas can boot against the same empty database at once.
    // The work lock coordinates healthy replicas without keeping a dedicated
    // database session open across Vault and KMS calls. Waiters keep checking
    // the permanent marker so they can finish as soon as the importer writes it.
    let Some(work_lock) =
        acquire_vault_import_work_lock(db_pool, work_lock_manager, cancel_token).await?
    else {
        tracing::info!("Vault import completed by another replica");
        return Ok(());
    };

    let import_result: eyre::Result<()> = async {
        // Strict enumeration: any list or read failure aborts the boot rather
        // than importing a subset and recording it as complete. The marker is
        // permanent, so a partial import here would be silent credential loss.
        let excluded_prefixes = if options.exclude_ufm {
            &[CredentialPrefix::UfmAuth][..]
        } else {
            &[]
        };
        let (secrets, excluded_ufm_prefix_found) = vault_client
            .get_secrets_strict_excluding_prefixes(excluded_prefixes)
            .await
            .map_err(eyre::Report::from)
            .wrap_err("enumerate vault secrets for import")?;
        validate_vault_import_selection(secrets.len(), excluded_ufm_prefix_found)?;

        tracing::info!(
            import_secret_count = secrets.len(),
            excluded_ufm_prefix_found,
            approach = ?options.secrets.import_approach,
            "Importing secrets from vault"
        );
        let stats = carbide_api_core::secrets::import_vault_secrets(
            db_pool,
            &work_lock,
            routing,
            kms,
            &secrets,
            options.secrets.import_approach,
        )
        .await
        .map_err(eyre::Report::new)
        .wrap_err("vault secret import")?;
        match stats {
            carbide_api_core::secrets::ImportResult::Completed { imported, skipped } => {
                tracing::info!(
                    imported_secret_count = imported,
                    skipped_secret_count = skipped,
                    "Vault secret import completed"
                );
            }
            carbide_api_core::secrets::ImportResult::AlreadyComplete => {
                tracing::info!("Vault import completed by another replica");
            }
        }

        Ok(())
    }
    .await;

    match (import_result, work_lock.release().await) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(import_error), Ok(())) => Err(import_error),
        (Ok(()), Err(release_error)) => {
            tracing::warn!(
                error = %release_error,
                "Vault import committed but its work lock could not be released"
            );
            Ok(())
        }
        (Err(import_error), Err(release_error)) => {
            tracing::warn!(
                error = %release_error,
                "Vault import failed and its work lock could not be released"
            );
            Err(import_error)
        }
    }
}

fn validate_vault_import_selection(
    eligible_secret_count: usize,
    excluded_prefix_found: bool,
) -> eyre::Result<()> {
    if eligible_secret_count == 0 && !excluded_prefix_found {
        return Err(eyre::eyre!(
            "vault enumeration returned no secrets; refusing to record an import from an empty vault. if this site really has no vault secrets, remove import_from from the [secrets] config; otherwise fix vault and restart"
        ));
    }

    Ok(())
}

async fn is_import_complete(db_pool: &PgPool) -> eyre::Result<bool> {
    carbide_api_core::secrets::is_vault_import_complete(db_pool)
        .await
        .map_err(eyre::Report::new)
        .wrap_err("check vault import status")
}

#[cfg(test)]
mod tests {
    use carbide_secrets::credentials::{CredentialKey, Credentials};

    use super::*;

    #[test]
    fn vault_import_allows_an_excluded_only_source() {
        for (name, eligible_secret_count, excluded_prefix_found, expected_ok) in [
            ("empty vault", 0, false, false),
            ("only excluded UFM credentials", 0, true, true),
            ("eligible credentials", 1, false, true),
        ] {
            assert_eq!(
                validate_vault_import_selection(eligible_secret_count, excluded_prefix_found)
                    .is_ok(),
                expected_ok,
                "{name}"
            );
        }
    }

    #[test]
    fn config_credentials_file_overrides_environment_selected_file_source() {
        let mut carbide_config = carbide_api_core::test_support::default_config::get();
        carbide_config.credentials.ufm_source = UfmCredentialSource::Local;
        carbide_config.credentials.file =
            Some(carbide_api_core::cfg::file::CredentialFileSourceConfig {
                path: "/var/run/secrets/nico/ufm/credentials.yaml".into(),
                poll_interval: std::time::Duration::from_secs(17),
            });
        let credential_config = CredentialConfig {
            file: carbide_secrets::FileCredentialsConfig {
                enabled: Some(true),
                path: Some("legacy-credentials.yaml".into()),
                poll_interval: Some(std::time::Duration::from_secs(23)),
            },
            ..Default::default()
        };

        let file_config = file_credentials_config(&carbide_config, &credential_config);

        assert!(file_config.enabled());
        assert_eq!(
            file_config.path(),
            std::path::PathBuf::from("/var/run/secrets/nico/ufm/credentials.yaml")
        );
        assert_eq!(
            file_config.poll_interval(),
            std::time::Duration::from_secs(17)
        );
    }

    #[tokio::test]
    async fn ufm_source_controls_legacy_file_read_precedence() {
        let dir = tempfile::tempdir().expect("create credential directory");
        let path = dir.path().join("credentials.json");
        let key = CredentialKey::UfmAuth {
            fabric: "default".to_string(),
        };
        let local_credentials = Credentials::UsernamePassword {
            username: "local-user".to_string(),
            password: "local-token".to_string(),
        };
        let backend_credentials = Credentials::UsernamePassword {
            username: "legacy-user".to_string(),
            password: "legacy-token".to_string(),
        };

        enum Expected {
            Local,
            Backend,
            Blocked,
        }
        let default_ufm_source =
            carbide_api_core::cfg::file::CredentialsConfig::default().ufm_source;
        let cases = [
            (
                "default preserves a legacy file override",
                r#"{
  "ufm_auth_by_fabric": {
    "default": {
      "username": "local-user",
      "password": "local-token"
    }
  }
}"#,
                default_ufm_source,
                Expected::Local,
            ),
            (
                "default falls back when the legacy file has no entry",
                "{}",
                default_ufm_source,
                Expected::Backend,
            ),
            (
                "authoritative file entry",
                r#"{
  "ufm_auth_by_fabric": {
    "default": {
      "username": "local-user",
      "password": "local-token"
    }
  }
}"#,
                UfmCredentialSource::Local,
                Expected::Local,
            ),
            (
                "authoritative local source miss",
                "{}",
                UfmCredentialSource::Local,
                Expected::Blocked,
            ),
            (
                "backend mode ignores local entry",
                r#"{
  "ufm_auth_by_fabric": {
    "default": {
      "username": "local-user",
      "password": "local-token"
    }
  }
}"#,
                UfmCredentialSource::Backend,
                Expected::Backend,
            ),
        ];

        for (name, file_contents, ufm_source, expected) in cases {
            tokio::fs::write(&path, file_contents)
                .await
                .unwrap_or_else(|error| panic!("{name}: write local credential snapshot: {error}"));
            let mut carbide_config = carbide_api_core::test_support::default_config::get();
            let ib_config = carbide_config.ib_config.get_or_insert_default();
            ib_config.enabled = true;
            carbide_config.credentials.ufm_source = ufm_source;
            let credential_config = CredentialConfig {
                env: carbide_secrets::EnvCredentialsConfig {
                    enabled: Some(false),
                    ..Default::default()
                },
                file: carbide_secrets::FileCredentialsConfig {
                    enabled: Some(true),
                    path: Some(path.clone()),
                    poll_interval: Some(std::time::Duration::from_secs(60)),
                },
                ..Default::default()
            };
            let mut readers =
                match local_credential_readers(&carbide_config, &credential_config).await {
                    Ok(readers) => {
                        assert!(
                            !matches!(expected, Expected::Blocked),
                            "{name}: local-source startup unexpectedly succeeded"
                        );
                        readers
                    }
                    Err(error) => {
                        assert!(
                            matches!(expected, Expected::Blocked),
                            "{name}: build local readers: {error}"
                        );
                        assert!(
                            error.to_string().contains("default")
                                && error.to_string().contains("credentials.ufm_source"),
                            "{name}: {error}"
                        );
                        continue;
                    }
                };
            let backend = MemoryCredentialStore::default();
            backend
                .set_credentials(&key, &backend_credentials)
                .await
                .expect("seed persistent backend");
            readers.push(Box::new(backend));
            let chain = carbide_secrets::ChainedCredentialReader::from(readers);

            let result = chain.get_credentials(&key).await;

            match expected {
                Expected::Local => assert_eq!(
                    result.unwrap_or_else(|error| panic!("{name}: file lookup failed: {error}")),
                    Some(local_credentials.clone()),
                    "{name}"
                ),
                Expected::Backend => assert_eq!(
                    result.unwrap_or_else(|error| panic!("{name}: backend lookup failed: {error}")),
                    Some(backend_credentials.clone()),
                    "{name}"
                ),
                Expected::Blocked => unreachable!("handled during startup validation"),
            }
        }
    }

    #[tokio::test]
    async fn local_ufm_validation_skips_disabled_ib_management() {
        let dir = tempfile::tempdir().expect("create credential directory");
        let path = dir.path().join("credentials.json");
        tokio::fs::write(&path, "{}")
            .await
            .expect("write empty credential snapshot");
        let mut carbide_config = carbide_api_core::test_support::default_config::get();
        carbide_config.ib_config = None;
        carbide_config.credentials.ufm_source = UfmCredentialSource::Local;
        carbide_config.credentials.file =
            Some(carbide_api_core::cfg::file::CredentialFileSourceConfig {
                path,
                poll_interval: std::time::Duration::from_secs(60),
            });
        let credential_config = CredentialConfig {
            env: carbide_secrets::EnvCredentialsConfig {
                enabled: Some(false),
                ..Default::default()
            },
            ..Default::default()
        };

        local_credential_readers(&carbide_config, &credential_config)
            .await
            .expect("disabled IB management must not require dormant UFM credentials");
    }

    #[tokio::test]
    async fn configured_ufm_source_controls_backend_mutations() {
        let key = CredentialKey::UfmAuth {
            fabric: "fabric-a".to_string(),
        };
        let credentials = Credentials::UsernamePassword {
            username: "ufm-user".to_string(),
            password: "ufm-token".to_string(),
        };
        let cases = [
            (
                "local ownership",
                UfmCredentialSource::Local,
                Some("local sources own all UFM credentials"),
            ),
            (
                "local-first compatibility ownership",
                UfmCredentialSource::LocalFirst,
                None,
            ),
            ("backend ownership", UfmCredentialSource::Backend, None),
        ];

        for (name, ufm_source, blocked_message) in cases {
            let mut carbide_config = carbide_api_core::test_support::default_config::get();
            carbide_config.credentials.ufm_source = ufm_source;
            let backend = Arc::new(MemoryCredentialStore::default());
            let writer = credential_writer_with_ufm_policy(&carbide_config, backend.clone());

            let result = writer.set_credentials(&key, &credentials).await;

            if let Some(blocked_message) = blocked_message {
                let error = result.expect_err("UFM backend mutation must be blocked");
                assert!(
                    matches!(
                        &error,
                        carbide_secrets::SecretsError::UfmCredentialMutationBlocked { .. }
                    ),
                    "{name}: {error}"
                );
                assert!(
                    error.to_string().contains(blocked_message),
                    "{name}: {error}"
                );
                assert_eq!(
                    backend
                        .get_credentials_from_writer(&key)
                        .await
                        .unwrap_or_else(|error| panic!("{name}: read backend: {error}")),
                    None,
                    "{name}"
                );
            } else {
                result.unwrap_or_else(|error| panic!("{name}: write backend: {error}"));
                assert_eq!(
                    backend
                        .get_credentials_from_writer(&key)
                        .await
                        .unwrap_or_else(|error| panic!("{name}: read backend: {error}")),
                    Some(credentials.clone()),
                    "{name}"
                );
            }
        }
    }

    /// The pool builder rejects zero-valued lifecycle settings before it
    /// touches the database, naming the offending field.
    #[tokio::test]
    async fn zero_database_pool_durations_are_rejected_at_startup() {
        type ZeroOut = fn(&mut CarbideConfig);
        let cases: [(&str, ZeroOut); 3] = [
            ("database_pool_acquire_timeout", |config| {
                config.database_pool_acquire_timeout = std::time::Duration::ZERO
            }),
            ("database_pool_idle_timeout", |config| {
                config.database_pool_idle_timeout = std::time::Duration::ZERO
            }),
            ("database_pool_max_lifetime", |config| {
                config.database_pool_max_lifetime = std::time::Duration::ZERO
            }),
        ];
        for (field, zero_out) in cases {
            let mut config = carbide_api_core::test_support::default_config::get();
            zero_out(&mut config);
            let error = connect_postgres(&config)
                .await
                .expect_err("a zero-valued pool duration must be rejected");
            assert!(
                error.to_string().contains(field),
                "error must name `{field}`, got: {error}"
            );
        }
    }

    fn unreachable_connect_options() -> sqlx::postgres::PgConnectOptions {
        // Nothing listens on this port on localhost, so the connection is
        // refused immediately instead of hanging on a real timeout.
        "postgres://user:pass@127.0.0.1:1/db"
            .parse()
            .expect("valid connection string")
    }

    fn bounded_pool_options() -> sqlx::pool::PoolOptions<sqlx::Postgres> {
        // Some environments don't refuse a closed low port immediately (e.g.
        // a firewall drops the SYN instead of sending RST), so each attempt
        // is bounded explicitly rather than relying on the OS to fail fast.
        sqlx::pool::PoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_millis(200))
    }

    /// A `retry_timeout` of zero preserves the old behavior: fail on the
    /// very first connection attempt, no retry, and the error doesn't
    /// falsely claim it retried.
    #[tokio::test]
    async fn connect_with_retry_fails_immediately_when_timeout_is_zero() {
        let start = std::time::Instant::now();
        let error = connect_with_retry(
            bounded_pool_options(),
            unreachable_connect_options(),
            std::time::Duration::ZERO,
        )
        .await
        .expect_err("an unreachable database must fail");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "a zero retry timeout must not wait between attempts"
        );
        assert!(
            !error.to_string().contains("retrying"),
            "a single, unretried attempt must not claim it retried, got: {error}"
        );
    }

    /// With a non-zero `retry_timeout`, a still-unreachable database keeps
    /// getting retried until the window elapses, then fails -- it does not
    /// give up on the first error, and it does not retry forever.
    #[tokio::test]
    async fn connect_with_retry_retries_until_timeout_then_fails() {
        let retry_timeout = std::time::Duration::from_millis(1500);
        let start = std::time::Instant::now();
        let error = connect_with_retry(
            bounded_pool_options(),
            unreachable_connect_options(),
            retry_timeout,
        )
        .await
        .expect_err("a database that never becomes reachable must eventually fail");
        assert!(
            start.elapsed() >= retry_timeout,
            "must keep retrying for the full window before giving up"
        );
        assert!(
            error.to_string().contains("retrying for"),
            "error should explain it gave up after retrying, got: {error}"
        );
    }

    /// A hostname that can never resolve (DNS failure, not just a refused
    /// TCP connection) is retried like any other connection error, and a
    /// lookup that hangs can't run a single attempt past the remaining
    /// window: total time stays close to `retry_timeout` either way.
    #[tokio::test]
    async fn connect_with_retry_bounds_a_dns_resolution_failure() {
        // ".invalid" is reserved by RFC 2606 to never resolve.
        let connect_options = "postgres://user:pass@nowhere.invalid/db"
            .parse::<sqlx::postgres::PgConnectOptions>()
            .expect("valid connection string");
        let retry_timeout = std::time::Duration::from_millis(1500);
        let start = std::time::Instant::now();
        let error = connect_with_retry(bounded_pool_options(), connect_options, retry_timeout)
            .await
            .expect_err("a hostname that never resolves must eventually fail");
        assert!(
            start.elapsed() >= retry_timeout
                && start.elapsed() < retry_timeout + std::time::Duration::from_secs(2),
            "must give up close to the retry window even if DNS resolution hangs, took {:?}",
            start.elapsed()
        );
        assert!(error.to_string().contains("retrying for"));
    }

    /// Once the retry window has fully elapsed, the loop gives up right
    /// away instead of sleeping for a full backoff delay and trying again --
    /// a `retry_timeout` shorter than the first backoff delay means the
    /// window is already gone by the time the loop would otherwise retry.
    #[tokio::test]
    async fn connect_with_retry_does_not_retry_once_the_window_is_already_gone() {
        let retry_timeout = std::time::Duration::from_millis(50);
        let start = std::time::Instant::now();
        let _ = connect_with_retry(
            bounded_pool_options(),
            unreachable_connect_options(),
            retry_timeout,
        )
        .await
        .expect_err("an unreachable database must fail");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "must give up as soon as the window closes rather than sleeping for a full \
             backoff delay first, took {:?}",
            start.elapsed()
        );
    }
}
