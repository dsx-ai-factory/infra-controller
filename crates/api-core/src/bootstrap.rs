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

//! Implementation interface for the top-level `carbide-api` composition crate.
//!
//! This is not a general-purpose library API. It groups the process-bootstrap
//! types that must cross the crate boundary while service implementation stays
//! in `carbide-api-core`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use carbide_rack::rms_node_type::warn_rms_node_descriptor_attribute_overrides;
use carbide_secrets::certificates::CertificateProvider;
use carbide_secrets::credentials::CredentialManager;
use db::work_lock_manager::{AcquireLockError, WorkLock, WorkLockManagerHandle};
use eyre::WrapErr;
use sqlx::PgPool;
#[cfg(test)]
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use crate::api::metrics::ApiMetricsEmitter;
use crate::cfg::file::{CarbideConfig, InitialObjectsConfig};
use crate::dynamic_settings::DynamicSettings;
use crate::listener::AdminUiRoutesBuilder;
pub use crate::logging::level_filter::{ActiveLevel, ReloadableFilter};
pub use crate::logging::setup::{Logging, dep_log_filter};
pub use crate::logging::stream::LogStreamLayer;
use crate::secrets::SecretsContext;

// Replicas only contend when they name the same work, so keep this key stable
// across releases.
const VAULT_IMPORT_WORK_KEY: &str = "secrets::import_vault_secrets_once";
const VAULT_IMPORT_LOCK_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Opaque core runtime state initialized before external resources.
#[doc(hidden)]
pub struct RuntimePrelude {
    dynamic_settings: DynamicSettings,
}

/// Starts the core runtime work that follows logging initialization and precedes
/// external resource initialization.
///
/// Process-wide settings derived from the validated configuration are installed
/// here, immediately before runtime consumers can observe them.
///
/// Rack-profile attribute override collisions are logged once here so the
/// configured tracing subscriber receives the diagnostics.
#[doc(hidden)]
pub fn start_runtime_prelude(
    carbide_config: &CarbideConfig,
    logging: Logging,
    join_set: &mut JoinSet<()>,
    cancel_token: &CancellationToken,
) -> RuntimePrelude {
    // Install process-wide settings only after the caller has finished loading
    // and validating the configuration.
    crate::init_tools(carbide_config.web_ui_sidebar_tools.clone());
    crate::init_site_name(carbide_config.sitename.clone());
    crate::init_logs_link_template(carbide_config.web_ui_logs_link_template.clone());
    db::host_naming::configure(carbide_config.host_naming_strategy);

    // These diagnostics require the tracing subscriber installed by the caller.
    warn_rms_node_descriptor_attribute_overrides(&carbide_config.rack_profiles);

    // Redact credentials before printing the config
    let print_config = carbide_config.redacted();

    tracing::info!(
        print_config = ?print_config,
        "Using configuration",
    );
    tracing::info!(
        worker_count = tokio::runtime::Handle::current().metrics().num_workers(),
        cpu_count = num_cpus::get(),
        tokio_worker_threads = %std::env::var("TOKIO_WORKER_THREADS").unwrap_or_else(|_| "UNSET".to_string()),
        "Tokio worker thread configuration",
    );

    let dynamic_settings = DynamicSettings {
        log_filter: logging.filter.clone(),
        site_explorer_enabled: carbide_config.site_explorer.enabled.clone(),
        create_machines: carbide_config.site_explorer.create_machines.clone(),
        bmc_proxy: carbide_config.site_explorer.bmc_proxy.clone(),
        tracing_enabled: logging.tracing_enabled,
        log_stream: logging.log_stream,
    };
    dynamic_settings.start_reset_task(
        join_set,
        crate::dynamic_settings::RESET_PERIOD,
        cancel_token.clone(),
    );

    tracing::info!(
        listen_address = carbide_config.listen.to_string(),
        build_version = carbide_version::v!(build_version),
        build_date = carbide_version::v!(build_date),
        rust_version = carbide_version::v!(rust_version),
        "Start carbide-api",
    );

    RuntimePrelude { dynamic_settings }
}

/// Prepared resources passed through the cross-crate runtime boundary.
#[doc(hidden)]
pub struct RuntimeInputs<'a> {
    pub carbide_config: Arc<CarbideConfig>,
    pub initial_objects: Option<InitialObjectsConfig>,
    pub meter: opentelemetry::metrics::Meter,
    pub per_object_metrics: Option<prometheus::Registry>,
    pub join_set: &'a mut JoinSet<()>,
    pub runtime_prelude: RuntimePrelude,
    pub credential_manager: Arc<dyn CredentialManager>,
    pub certificate_provider: Arc<dyn CertificateProvider>,
    pub db_pool: PgPool,
    pub work_lock_manager_handle: WorkLockManagerHandle,
    pub secrets_context: Option<SecretsContext>,
    pub admin_ui_routes_builder: Option<AdminUiRoutesBuilder>,
    pub cancel_token: CancellationToken,
}

/// Enter api-core's private service runtime with fully prepared resources.
///
/// `admin_ui_routes_builder` is how the admin web UI's pages (everything under
/// `/admin`) get plugged in: pass `Some(Box::new(carbide_api_web::routes))` to
/// serve them, or `None` to skip the web UI entirely (e.g. in-process test
/// servers, which only hit the gRPC API). It's passed in rather than called
/// directly to avoid a dependency cycle — see [`AdminUiRoutesBuilder`] for why.
///
/// Note: even when `Some` is passed, the admin UI is only mounted if the
/// `enable_admin_ui` config flag is true (the default). When it's false, the
/// core runtime drops the builder and serves gRPC only — so `Some` here means
/// "offer the UI", not "force it on". The flag also gates the log-stream
/// layer feeding the UI's live log viewer: with the UI off, no per-event
/// work is spent collecting lines nothing can read.
///
/// Returns the effective API listener address after startup completes.
#[doc(hidden)]
pub async fn start_runtime(inputs: RuntimeInputs<'_>) -> eyre::Result<SocketAddr> {
    let RuntimeInputs {
        carbide_config,
        initial_objects,
        meter,
        per_object_metrics,
        join_set,
        runtime_prelude,
        credential_manager,
        certificate_provider,
        db_pool,
        work_lock_manager_handle,
        secrets_context,
        admin_ui_routes_builder,
        cancel_token,
    } = inputs;
    let RuntimePrelude { dynamic_settings } = runtime_prelude;

    crate::setup::start_runtime(
        join_set,
        carbide_config,
        initial_objects,
        meter,
        per_object_metrics,
        dynamic_settings,
        credential_manager,
        certificate_provider,
        db_pool,
        work_lock_manager_handle,
        secrets_context,
        admin_ui_routes_builder,
        cancel_token,
    )
    .await
}

/// Wait for the work lock that serializes the one-time Vault import.
///
/// `None` means another replica wrote the permanent completion marker while
/// this caller waited. Cancellation is only observed between acquisition
/// attempts: dropping an in-flight acquisition future could leave a leased row
/// without returning the [`WorkLock`] that releases it.
pub async fn acquire_vault_import_work_lock(
    db_pool: &PgPool,
    work_lock_manager: &WorkLockManagerHandle,
    cancel_token: &CancellationToken,
) -> eyre::Result<Option<WorkLock>> {
    #[cfg(not(test))]
    {
        acquire_vault_import_work_lock_with_retry(
            db_pool,
            work_lock_manager,
            cancel_token,
            VAULT_IMPORT_LOCK_RETRY_INTERVAL,
        )
        .await
    }
    #[cfg(test)]
    {
        acquire_vault_import_work_lock_with_retry(
            db_pool,
            work_lock_manager,
            cancel_token,
            VAULT_IMPORT_LOCK_RETRY_INTERVAL,
            None,
        )
        .await
    }
}

async fn acquire_vault_import_work_lock_with_retry(
    db_pool: &PgPool,
    work_lock_manager: &WorkLockManagerHandle,
    cancel_token: &CancellationToken,
    retry_interval: Duration,
    #[cfg(test)] contention_signal: Option<&Notify>,
) -> eyre::Result<Option<WorkLock>> {
    let mut logged_contention = false;
    loop {
        if cancel_token.is_cancelled() {
            eyre::bail!("vault import canceled while waiting for its work lock");
        }

        match work_lock_manager
            .try_acquire_lock(VAULT_IMPORT_WORK_KEY.into())
            .await
        {
            Ok(work_lock) => {
                if cancel_token.is_cancelled() {
                    if let Err(error) = work_lock.release().await {
                        tracing::warn!(
                            error = %error,
                            "Vault import was canceled but its work lock could not be released"
                        );
                    }
                    eyre::bail!("vault import canceled after acquiring its work lock");
                }

                let marker_status = crate::secrets::is_vault_import_complete(db_pool)
                    .await
                    .wrap_err("check vault import status after acquiring its work lock");
                let import_complete = match marker_status {
                    Ok(import_complete) => import_complete,
                    Err(marker_error) => {
                        if let Err(error) = work_lock.release().await {
                            tracing::warn!(
                                error = %error,
                                "Vault import marker check failed and its work lock could not be released"
                            );
                        }
                        return Err(marker_error);
                    }
                };
                if import_complete {
                    if let Err(error) = work_lock.release().await {
                        tracing::warn!(
                            %error,
                            "Vault import is complete but its work lock could not be released"
                        );
                    }
                    return Ok(None);
                }
                if cancel_token.is_cancelled() {
                    if let Err(error) = work_lock.release().await {
                        tracing::warn!(
                            error = %error,
                            "Vault import was canceled but its work lock could not be released"
                        );
                    }
                    eyre::bail!("vault import canceled after acquiring its work lock");
                }
                return Ok(Some(work_lock));
            }
            Err(AcquireLockError::WorkAlreadyLocked(_)) => {
                if !logged_contention {
                    tracing::info!("Vault import is running on another replica; waiting");
                    logged_contention = true;
                }
                #[cfg(test)]
                if let Some(contention_signal) = contention_signal {
                    contention_signal.notify_one();
                }
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        eyre::bail!("vault import canceled while waiting for its work lock");
                    }
                    _ = tokio::time::sleep(retry_interval) => {}
                }
                if crate::secrets::is_vault_import_complete(db_pool)
                    .await
                    .wrap_err("check vault import status while waiting for its work lock")?
                {
                    return Ok(None);
                }
            }
            Err(error) => {
                return Err(error).wrap_err("acquire vault import work lock");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_RETRY_INTERVAL: Duration = Duration::from_millis(10);

    async fn start_test_work_lock_manager(pool: &PgPool) -> (WorkLockManagerHandle, JoinSet<()>) {
        let mut tasks = JoinSet::new();
        let handle = db::work_lock_manager::start(&mut tasks, pool.clone(), Default::default())
            .await
            .expect("start work lock manager");
        (handle, tasks)
    }

    async fn write_vault_import_marker(pool: &PgPool) {
        // Lock coordination only checks that the reserved path exists, so
        // these fields do not need to contain an encrypted marker value.
        let bytes = b"test";
        let mut connection = pool.acquire().await.expect("acquire marker connection");
        db::secrets::insert(
            &mut connection,
            &db::secrets::NewSecretEntry {
                path: crate::secrets::VAULT_IMPORT_MARKER_PATH,
                encrypted_value: bytes,
                nonce: bytes,
                kek_id: "test",
                encrypted_dek: bytes,
                dek_nonce: bytes,
            },
        )
        .await
        .expect("write vault import marker");
    }

    fn spawn_waiter(
        pool: PgPool,
        work_lock_manager: WorkLockManagerHandle,
        cancel_token: CancellationToken,
        contention_signal: Arc<Notify>,
    ) -> tokio::task::JoinHandle<eyre::Result<Option<WorkLock>>> {
        tokio::spawn(async move {
            acquire_vault_import_work_lock_with_retry(
                &pool,
                &work_lock_manager,
                &cancel_token,
                TEST_RETRY_INTERVAL,
                Some(&contention_signal),
            )
            .await
        })
    }

    async fn wait_for_contention(contention_signal: &Notify) {
        tokio::time::timeout(Duration::from_secs(3), contention_signal.notified())
            .await
            .expect("waiter did not observe lock contention");
    }

    // A replica that acquires the work key must re-check the marker before it
    // starts Vault I/O, then release the unused key.
    #[crate::sqlx_test]
    async fn vault_import_owner_observes_completion_marker(pool: PgPool) {
        let (work_lock_manager, _work_lock_tasks) = start_test_work_lock_manager(&pool).await;
        write_vault_import_marker(&pool).await;

        let work_lock = acquire_vault_import_work_lock_with_retry(
            &pool,
            &work_lock_manager,
            &CancellationToken::new(),
            TEST_RETRY_INTERVAL,
            None,
        )
        .await
        .expect("check marker after acquiring vault import lock");
        assert!(
            work_lock.is_none(),
            "a completed import must not retain the work key"
        );

        let work_lock = work_lock_manager
            .try_acquire_lock(VAULT_IMPORT_WORK_KEY.into())
            .await
            .expect("marker check must release the work key");
        work_lock
            .release()
            .await
            .expect("release verification lock");
    }

    // A waiting replica can finish as soon as the importer writes its marker;
    // it does not need the owner to release the work key first.
    #[crate::sqlx_test]
    async fn vault_import_waiter_observes_completion_marker(pool: PgPool) {
        let (owner_manager, _owner_tasks) = start_test_work_lock_manager(&pool).await;
        let (waiter_manager, _waiter_tasks) = start_test_work_lock_manager(&pool).await;
        let owner = owner_manager
            .try_acquire_lock(VAULT_IMPORT_WORK_KEY.into())
            .await
            .expect("acquire owner lock");
        let contention_signal = Arc::new(Notify::new());

        let waiter = spawn_waiter(
            pool.clone(),
            waiter_manager.clone(),
            CancellationToken::new(),
            contention_signal.clone(),
        );
        wait_for_contention(&contention_signal).await;
        write_vault_import_marker(&pool).await;

        let work_lock = tokio::time::timeout(Duration::from_secs(3), waiter)
            .await
            .expect("waiter did not observe the marker")
            .expect("waiter task panicked")
            .expect("wait for vault import lock");
        assert!(
            work_lock.is_none(),
            "a completed import must not acquire the work key"
        );
        assert!(
            matches!(
                waiter_manager
                    .try_acquire_lock(VAULT_IMPORT_WORK_KEY.into())
                    .await,
                Err(AcquireLockError::WorkAlreadyLocked(_))
            ),
            "owner must retain the work key while the waiter observes the marker"
        );

        owner.release().await.expect("release owner lock");
    }

    // If the owner exits before writing the marker, its Drop queues a release
    // and the waiting replica can acquire the same work key on its next retry.
    #[crate::sqlx_test]
    async fn vault_import_waiter_retries_after_owner_drops(pool: PgPool) {
        let (owner_manager, _owner_tasks) = start_test_work_lock_manager(&pool).await;
        let (waiter_manager, _waiter_tasks) = start_test_work_lock_manager(&pool).await;
        let owner = owner_manager
            .try_acquire_lock(VAULT_IMPORT_WORK_KEY.into())
            .await
            .expect("acquire owner lock");
        let contention_signal = Arc::new(Notify::new());

        let waiter = spawn_waiter(
            pool.clone(),
            waiter_manager,
            CancellationToken::new(),
            contention_signal.clone(),
        );
        wait_for_contention(&contention_signal).await;

        drop(owner);
        let work_lock = tokio::time::timeout(Duration::from_secs(3), waiter)
            .await
            .expect("waiter did not retry")
            .expect("waiter task panicked")
            .expect("wait for vault import lock")
            .expect("waiter must acquire the released work key");
        work_lock.release().await.expect("release waiter lock");
    }

    // Cancellation stops the retry sleep, but never drops an in-flight
    // acquisition future that could have inserted a `work_locks` row.
    #[crate::sqlx_test]
    async fn vault_import_waiter_stops_after_cancellation(pool: PgPool) {
        let (owner_manager, _owner_tasks) = start_test_work_lock_manager(&pool).await;
        let (waiter_manager, _waiter_tasks) = start_test_work_lock_manager(&pool).await;
        let owner = owner_manager
            .try_acquire_lock(VAULT_IMPORT_WORK_KEY.into())
            .await
            .expect("acquire owner lock");
        let cancel_token = CancellationToken::new();
        let contention_signal = Arc::new(Notify::new());

        let waiter = spawn_waiter(
            pool,
            waiter_manager,
            cancel_token.clone(),
            contention_signal.clone(),
        );
        wait_for_contention(&contention_signal).await;
        cancel_token.cancel();

        let error = tokio::time::timeout(Duration::from_secs(3), waiter)
            .await
            .expect("waiter ignored cancellation")
            .expect("waiter task panicked")
            .err()
            .expect("cancellation must stop the waiter");
        assert!(
            error.to_string().contains("vault import canceled"),
            "unexpected error: {error}"
        );

        owner.release().await.expect("release owner lock");
    }
}
