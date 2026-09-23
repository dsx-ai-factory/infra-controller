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

use std::net::SocketAddr;
use std::path::PathBuf;

use carbide_api_core::AdminUiRoutesBuilder;
use carbide_api_core::bootstrap::{Logging, RuntimeInputs, start_runtime, start_runtime_prelude};
use carbide_secrets::CredentialConfig;
use eyre::WrapErr;
use ipnetwork::IpNetwork;
use tokio::net::TcpListener;
use tokio::sync::oneshot::Sender;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::subscriber::NoSubscriber;

use crate::logging::setup_logging;
use crate::metrics::{Metrics, setup_metrics};
use crate::resources::{RuntimeResources, setup_resources};

/// Effective addresses owned by a running carbide-api server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiServerAddresses {
    /// Address of the API listener.
    pub listen_address: SocketAddr,
    /// Address of the metrics listener, when configured.
    pub metrics_address: Option<SocketAddr>,
}

/// Run the carbide-api server until `cancel_token` is cancelled or the process
/// receives a shutdown signal.
///
/// Once startup completes, `ready_channel` receives the effective API and metrics listener
/// addresses. This includes OS-selected ports when either endpoint is configured with port zero.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    debug: u8,
    config_path: PathBuf,
    site_config_path: Option<PathBuf>,
    credential_config: CredentialConfig,
    skip_logging_setup: bool,
    admin_ui_routes_builder: Option<AdminUiRoutesBuilder>,
    cancel_token: CancellationToken,
    ready_channel: Sender<ApiServerAddresses>,
) -> eyre::Result<()> {
    let carbide_config = carbide_api_core::cfg::load::parse_carbide_config(
        &config_path,
        site_config_path.as_deref(),
    )?;

    // If `CarbideConfig.initial_objects_file` is set, load it into an
    // `InitialObjectsConfig` so that the core runtime can reconcile its contents
    // against the database on first startup.
    let initial_objects = if let Some(path) = carbide_config.initial_objects_file.as_deref() {
        Some(
            carbide_api_core::cfg::load::parse_initial_objects_config_with_policy(
                path,
                carbide_config.deny_unknown_fields,
            )?,
        )
    } else {
        None
    };

    validate_network_prefixes(&carbide_config)?;

    let log_history_max_bytes = carbide_config
        .log_history
        .max_megabytes
        .saturating_mul(1024 * 1024);
    let logging = if skip_logging_setup {
        Logging::default()
    } else {
        setup_logging(
            debug,
            carbide_machine_controller::extra_logfmt_logging_fields(),
            None::<NoSubscriber>,
            log_history_max_bytes,
            carbide_config.enable_admin_ui,
            &carbide_config.tracing,
        )
        .wrap_err("setup_telemetry")?
    };

    carbide_api_core::cfg::load::log_vpc_peering_policy_deprecations(&carbide_config);

    let Metrics {
        registry,
        meter,
        _meter_provider,
    } = setup_metrics(logging.spancount_reader.clone())?;

    // All background tasks that run "forever" (until canceled) are added to this JoinSet. When
    // initialization is complete, we use [`JoinSet::join_all`] to wait for them all to complete,
    // while propagating any panics to the current task.
    let mut join_set = JoinSet::new();
    crate::shutdown_handler::start(&mut join_set, cancel_token.clone());
    let metrics_address = start_metrics_endpoint(
        &mut join_set,
        &carbide_config,
        registry,
        cancel_token.clone(),
    )
    .await?;
    let per_object_metrics =
        start_per_object_metrics_endpoint(&mut join_set, &carbide_config, cancel_token.clone())?;

    let runtime_prelude =
        start_runtime_prelude(&carbide_config, logging, &mut join_set, &cancel_token);

    let RuntimeResources {
        credential_manager,
        certificate_provider,
        db_pool,
        work_lock_manager_handle,
        secrets_context,
    } = setup_resources(
        &carbide_config,
        &credential_config,
        &mut join_set,
        &cancel_token,
    )
    .await?;

    let listen_address = start_runtime(RuntimeInputs {
        carbide_config,
        initial_objects,
        meter,
        per_object_metrics,
        join_set: &mut join_set,
        runtime_prelude,
        credential_manager,
        certificate_provider,
        db_pool,
        work_lock_manager_handle,
        secrets_context,
        admin_ui_routes_builder,
        cancel_token,
    })
    .await?;

    if ready_channel
        .send(ApiServerAddresses {
            listen_address,
            metrics_address,
        })
        .is_err()
    {
        tracing::warn!(
            "Bug: api server ready_channel is closed, could not notify readiness status"
        );
    }

    // Block forever until all spawned tasks complete. Any panics in spawned tasks will be
    // propagated here.
    join_set.join_all().await;
    Ok(())
}

async fn start_metrics_endpoint(
    join_set: &mut JoinSet<()>,
    carbide_config: &carbide_api_core::cfg::file::CarbideConfig,
    registry: prometheus::Registry,
    cancel_token: CancellationToken,
) -> eyre::Result<Option<SocketAddr>> {
    let Some(metrics_address) = carbide_config.metrics_endpoint else {
        return Ok(None);
    };

    let listener = TcpListener::bind(metrics_address)
        .await
        .wrap_err_with(|| format!("could not bind metrics endpoint at {metrics_address}"))?;
    let metrics_address = listener
        .local_addr()
        .wrap_err("could not read metrics endpoint address")?;

    tracing::info!(%metrics_address, "Starting metrics listener");

    // Spin up the web server which serves `/metrics` requests
    // If a replacement prefix for "carbide_" is configured, also emit metrics under that
    let additional_prefix =
        carbide_config
            .alt_metric_prefix
            .clone()
            .map(|alt| metrics_endpoint::PrefixMigration {
                old: "carbide_".to_string(),
                new: alt,
            });
    join_set
        .build_task()
        .name("metrics_endpoint")
        .spawn(async move {
            if let Err(error) = metrics_endpoint::run_metrics_endpoint_with_listener(
                &metrics_endpoint::MetricsEndpointConfig {
                    address: metrics_address,
                    registry,
                    health_controller: None,
                    additional_prefix,
                },
                cancel_token,
                listener,
            )
            .await
            {
                tracing::error!(
                    metrics_address = %metrics_address,
                    error = %error,
                    "Metrics endpoint failed",
                );
            }
        })?;

    Ok(Some(metrics_address))
}

/// Starts the dedicated listener for the opt-in per-object state metrics and
/// returns their bare Prometheus registry (`None` when disabled). Per-object
/// series are native pull collectors on their own registry — not
/// OpenTelemetry instruments, whose per-stream cardinality limit a per-object
/// fleet vastly exceeds — and their own endpoint, so operators can scrape (or
/// skip) them independently. No alt-prefix mirroring here: it would double
/// every per-object family.
fn start_per_object_metrics_endpoint(
    join_set: &mut JoinSet<()>,
    carbide_config: &carbide_api_core::cfg::file::CarbideConfig,
    cancel_token: CancellationToken,
) -> eyre::Result<Option<prometheus::Registry>> {
    let per_object_config = &carbide_config.observability.per_object_state_metrics;
    if per_object_config.enabled && per_object_config.object_types.is_empty() {
        tracing::warn!(
            "observability.per_object_state_metrics.enabled is set but object_types is empty; \
             not starting the per-object metrics endpoint"
        );
    }
    let per_object_metrics = (per_object_config.enabled
        && !per_object_config.object_types.is_empty())
    .then(prometheus::Registry::new);
    if let Some(registry) = &per_object_metrics {
        let address = per_object_config.listen_address;
        join_set
            .build_task()
            .name("per_object_metrics_endpoint")
            .spawn({
                let registry = registry.clone();
                async move {
                    if let Err(error) = metrics_endpoint::run_metrics_endpoint_with_cancellation(
                        &metrics_endpoint::MetricsEndpointConfig {
                            address,
                            registry,
                            health_controller: None,
                            additional_prefix: None,
                        },
                        cancel_token,
                    )
                    .await
                    {
                        tracing::error!(
                            per_object_metrics_address = %address,
                            error = %error,
                            "Per-object metrics endpoint failed",
                        );
                    }
                }
            })?;
    }
    Ok(per_object_metrics)
}

/// Returns whether two CIDR prefixes claim any of the same addresses.
///
/// Prefixes within one address family are nested or disjoint, so checking both network addresses
/// covers either containment direction. `IpNetwork::contains` rejects cross-family addresses.
fn prefixes_overlap(left: IpNetwork, right: IpNetwork) -> bool {
    left.contains(right.network()) || right.contains(left.network())
}

fn validate_network_prefixes(
    carbide_config: &carbide_api_core::cfg::file::CarbideConfig,
) -> eyre::Result<()> {
    // Reject config that contains overlaps between deny_prefixes and site_fabric_prefixes.
    for deny_prefix in &carbide_config.deny_prefixes {
        for site_fabric_prefix in &carbide_config.site_fabric_prefixes {
            if prefixes_overlap(*deny_prefix, *site_fabric_prefix) {
                return Err(eyre::eyre!(
                    "overlap found in deny_prefixes `{deny_prefix}` and site_fabric_prefixes \
                     `{site_fabric_prefix}`",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    struct PrefixPair {
        deny: &'static str,
        site_fabric: &'static str,
    }

    #[test]
    fn deny_site_fabric_overlap_is_address_family_aware() {
        value_scenarios!(run = |pair: PrefixPair| {
            prefixes_overlap(
                pair.deny.parse().expect("valid deny prefix"),
                pair.site_fabric.parse().expect("valid site fabric prefix"),
            )
        };
            "IPv4 deny prefix contains site fabric prefix" {
                PrefixPair {
                    deny: "10.0.0.0/8",
                    site_fabric: "10.20.0.0/16",
                } => true,
            }

            "IPv4 site fabric prefix contains deny prefix" {
                PrefixPair {
                    deny: "10.20.0.0/16",
                    site_fabric: "10.0.0.0/8",
                } => true,
            }

            "IPv6 deny prefix contains site fabric prefix" {
                PrefixPair {
                    deny: "2001:db8::/32",
                    site_fabric: "2001:db8:20::/48",
                } => true,
            }

            "identical IPv6 prefixes overlap" {
                PrefixPair {
                    deny: "2001:db8:20::/48",
                    site_fabric: "2001:db8:20::/48",
                } => true,
            }

            "IPv4 prefixes are disjoint" {
                PrefixPair {
                    deny: "10.0.0.0/8",
                    site_fabric: "192.0.2.0/24",
                } => false,
            }

            "IPv6 prefixes are disjoint" {
                PrefixPair {
                    deny: "2001:db8::/32",
                    site_fabric: "2001:db9::/32",
                } => false,
            }

            "IPv4 deny and IPv6 site fabric prefixes are separate" {
                PrefixPair {
                    deny: "10.0.0.0/8",
                    site_fabric: "2001:db8::/32",
                } => false,
            }

            "IPv6 deny and IPv4 site fabric prefixes are separate" {
                PrefixPair {
                    deny: "2001:db8::/32",
                    site_fabric: "10.0.0.0/8",
                } => false,
            }
        );
    }
}
