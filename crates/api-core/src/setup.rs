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
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use carbide_dpa::DpaInfo;
use carbide_dpa_manager::DpaMonitor;
use carbide_dpf::DpuDeploymentType;
use carbide_extension_service_controller::context::ExtensionServiceStateHandlerServices;
use carbide_extension_service_controller::handler::ExtensionServiceStateHandler;
use carbide_extension_service_controller::io::ExtensionServiceStateControllerIO;
use carbide_firmware::FirmwareDownloader;
use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_ib_fabric::IbFabricMonitor;
use carbide_ib_fabric::ib::{self, IBFabricManager};
use carbide_ib_partition_controller::context::IBPartitionStateHandlerServices;
use carbide_ib_partition_controller::handler::IBPartitionStateHandler;
use carbide_ib_partition_controller::io::IBPartitionStateControllerIO;
use carbide_ipmi::IPMITool;
use carbide_machine_controller::context::MachineStateHandlerServices;
use carbide_machine_controller::dpf::{
    CarbideBmcPasswordProvider, CarbideDPFLabeler, DpfOperations, DpfSdkOps,
};
use carbide_machine_controller::handler::MachineStateHandlerBuilder;
use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_machine_controller::per_object::MachinePerObjectInfo;
use carbide_network_segment_controller::context::NetworkSegmentStateHandlerServices;
use carbide_network_segment_controller::handler::NetworkSegmentStateHandler;
use carbide_network_segment_controller::io::NetworkSegmentStateControllerIO;
use carbide_nvlink_manager::{NvLinkManager, NvLinkManagerArgs};
use carbide_power_shelf_controller::context::PowerShelfStateHandlerServices;
use carbide_power_shelf_controller::handler::PowerShelfStateHandler;
use carbide_power_shelf_controller::io::PowerShelfStateControllerIO;
use carbide_preingestion_manager::PreingestionManager;
use carbide_rack::bms_client::BmsDsxExchangeHandle;
use carbide_rack_controller::config::RackConfig;
use carbide_rack_controller::context::RackStateHandlerServices;
use carbide_rack_controller::handler::RackStateHandler;
use carbide_rack_controller::io::RackStateControllerIO;
use carbide_redfish::libredfish::{BmcCredentialOps, RedfishClientPool};
use carbide_secrets::certificates::CertificateProvider;
use carbide_secrets::credentials::{CredentialManager, CredentialReader};
use carbide_site_explorer::{AuthenticatedBmcClient, EndpointExplorationService, SiteExplorer};
use carbide_spdm_controller::context::SpdmStateHandlerServices;
use carbide_spdm_controller::handler::SpdmAttestationStateHandler;
use carbide_spdm_controller::io::SpdmStateControllerIO;
use carbide_switch_controller::context::SwitchStateHandlerServices;
use carbide_switch_controller::handler::SwitchStateHandler;
use carbide_switch_controller::io::SwitchStateControllerIO;
use carbide_utils::HostPortPair;
use carbide_vpc_prefix_controller::context::VpcPrefixStateHandlerServices;
use carbide_vpc_prefix_controller::handler::VpcPrefixStateHandler;
use carbide_vpc_prefix_controller::io::VpcPrefixStateControllerIO;
use db::Transaction;
use db::machine::{update_dpu_asns, update_dpu_loopback_ips_v6};
use db::resource_pool::DefineResourcePoolError;
use db::work_lock_manager::WorkLockManagerHandle;
use eyre::WrapErr;
use futures_util::TryFutureExt;
use itertools::Itertools;
use librms::RackManagerClientPool;
use model::attestation::spdm::VerifierImpl;
use model::expected_machine::ExpectedMachine;
use model::ib::DEFAULT_IB_FABRIC_NAME;
use model::machine::HostHealthConfig;
use model::network_segment::NetworkDefinition;
use model::resource_pool::{self, ResourcePoolDef};
use model::route_server::RouteServerSourceType;
use model::vpc::VpcDefinition;
use opentelemetry::metrics::Meter;
use sqlx::PgPool;
use state_controller::controller::{Enqueuer, StateController};
use state_controller::per_object::{PerObjectStateMetrics, PerObjectStateRecorder};
use state_controller::state_change_emitter::StateChangeEmitterBuilder;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::api::Api;
use crate::api::metrics::ApiMetricsEmitter;
use crate::cfg::file::{CarbideConfig, InitialObjectsConfig, ListenMode, VmaasConfig};
use crate::cfg::load::all_configuration_files;
use crate::dpa::handler::start_svpc_handler;
use crate::dynamic_settings::DynamicSettings;
use crate::handlers::machine_validation::apply_config_on_startup;
use crate::listener::{AdminUiRoutesBuilder, ApiListenMode};
use crate::logging::log_limiter::LogLimiter;
use crate::logging::service_health_metrics::{
    ServiceHealthContext, start_export_service_health_metrics,
};
use crate::machine_update_manager::MachineUpdateManager;
use crate::measured_boot::metrics_collector::MeasuredBootMetricsCollector;
use crate::mqtt_state_change_hook::hook::MqttStateChangeHook;
use crate::mqtt_state_change_hook::republisher::{
    ManagedHostStateRepublisher, ManagedHostStateRepublisherParams,
};
use crate::scout_stream::ConnectionRegistry;
use crate::{CarbideError, attestation, db_init, ethernet_virtualization, listener};

fn create_ipmi_tool(
    credential_reader: Arc<dyn CredentialReader>,
    carbide_config: &CarbideConfig,
    bmc_proxy: Arc<ArcSwap<Option<HostPortPair>>>,
) -> Arc<dyn IPMITool> {
    match carbide_config.dpu_ipmi_tool_impl.as_deref() {
        Some("test") => {
            tracing::info!("Disabling ipmitool");
            carbide_ipmi::test_support()
        }
        Some("bmc-mock") => {
            tracing::info!("Using HTTP IPMI transport via bmc_proxy");
            carbide_ipmi::bmc_mock(bmc_proxy, credential_reader)
        }
        _ => {
            tracing::info!("Using lanplus IPMI transport (/usr/bin/ipmitool)");
            carbide_ipmi::tool(credential_reader, carbide_config.dpu_ipmi_reboot_attempts)
        }
    }
}

fn create_redfish_pool(
    carbide_config: &CarbideConfig,
    credential_manager: Arc<dyn CredentialManager>,
) -> eyre::Result<(Arc<dyn RedfishClientPool>, Arc<dyn BmcCredentialOps>)> {
    let pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()
        .map_err(CarbideError::from)?;

    // Support deprecated configuration for site_explorer.override_target_ip and
    // override_target_port. Configuration should migrate to site_explorer.bmc_proxy.
    match (
        &carbide_config.site_explorer.override_target_ip,
        carbide_config.site_explorer.override_target_port,
        carbide_config.site_explorer.bmc_proxy.load().as_ref(),
    ) {
        (Some(_), _, Some(_)) => tracing::warn!(
            "Ignoring deprecated config site_explorer.override_target_ip, since site_explorer.bmc_proxy is also set. Please delete override_target_ip from site_explorer config."
        ),
        (Some(ip), port, None) => {
            tracing::warn!(
                "Deprecated site_explorer.override_target_ip in carbide config. Setting site_explorer.bmc_proxy instead. Please migrate configuration."
            );
            let proxy = port.map_or_else(
                || HostPortPair::HostOnly(ip.to_string()),
                |port| HostPortPair::HostAndPort(ip.to_string(), port),
            );
            carbide_config
                .site_explorer
                .bmc_proxy
                .store(Arc::new(Some(proxy)));
        }
        (None, Some(port), None) => {
            tracing::warn!(
                "Deprecated site_explorer.override_target_port in carbide config. Setting site_explorer.bmc_proxy instead. Please migrate configuration."
            );
            carbide_config
                .site_explorer
                .bmc_proxy
                .store(Arc::new(Some(HostPortPair::PortOnly(port))));
        }
        (None, Some(_), Some(_)) => tracing::warn!(
            "Ignoring deprecated config site_explorer.override_target_port, since site_explorer.bmc_proxy is also set. Please delete override_target_port from site_explorer config."
        ),
        (None, None, _) => {} // leave bmc_proxy untouched
    }

    Ok(carbide_redfish::libredfish::new_pool_with_credential_ops(
        credential_manager,
        pool,
        carbide_config.site_explorer.bmc_proxy.clone(),
    ))
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all)]
pub(crate) async fn start_runtime(
    join_set: &mut JoinSet<()>,
    carbide_config: Arc<CarbideConfig>,
    initial_objects: Option<InitialObjectsConfig>,
    meter: Meter,
    per_object_prometheus_registry: Option<prometheus::Registry>,
    dynamic_settings: DynamicSettings,
    credential_manager: Arc<dyn CredentialManager>,
    certificate_provider: Arc<dyn CertificateProvider>,
    db_pool: PgPool,
    work_lock_manager_handle: WorkLockManagerHandle,
    secrets_context: Option<crate::secrets::SecretsContext>,
    admin_ui_routes_builder: Option<AdminUiRoutesBuilder>,
    cancel_token: CancellationToken,
) -> eyre::Result<SocketAddr> {
    let (shared_redfish_pool, bmc_credential_ops) =
        create_redfish_pool(&carbide_config, credential_manager.clone())?;
    let shared_nv_redfish_pool =
        carbide_redfish::nv_redfish::new_pool(carbide_config.site_explorer.bmc_proxy.clone());

    let ipmi_tool = create_ipmi_tool(
        credential_manager.clone(),
        &carbide_config,
        dynamic_settings.bmc_proxy.clone(),
    );

    let (rms_client, switch_system_image_rms_api) = match carbide_config.rms.api_url.clone() {
        Some(url) if !url.is_empty() => {
            let rms_client_config = librms::client_config::RmsClientConfig::new(
                carbide_config.rms.root_ca_path.clone(),
                carbide_config.rms.client_cert.clone(),
                carbide_config.rms.client_key.clone(),
                carbide_config.rms.enforce_tls,
            );
            let rms_api_config = librms::client::RmsApiConfig::new(&url, &rms_client_config);
            let rms_client_pool = librms::RmsClientPool::new(&rms_api_config);
            let shared_rms_client = rms_client_pool.create_client().await;
            let switch_system_image_rms_api =
                Arc::new(librms::RackManagerApi::new(&rms_api_config));
            (Some(shared_rms_client), Some(switch_system_image_rms_api))
        }
        _ => (None, None),
    };
    let ib_config = carbide_config.ib_config.clone().unwrap_or_default();
    let fabric_manager_type = match ib_config.enabled {
        true => ib::IBFabricManagerType::Rest,
        false => ib::IBFabricManagerType::Disable,
    };

    let ib_fabric_ids = match ib_config.enabled {
        false => HashSet::new(),
        true => carbide_config.ib_fabrics.keys().cloned().collect(),
    };

    // Resolve initial seed data up-front so any configuration conflicts surface
    // before we touch the database. The actual reconcile/creation runs inside
    // `initialize_and_start_controllers`.
    let seed_data = if carbide_config.listen_only {
        tracing::info!("Not populating initial seed data in database, as listen_only=true");
        None
    } else {
        Some(SeedData::resolve(
            &carbide_config,
            initial_objects.as_ref(),
        )?)
    };

    // Note: Normally we want initialize_and_start_controllers to be responsible for populating
    // information into the database, but resource pools, route servers, and configured site
    // prefixes need to be defined first. The controllers rely on a fully-hydrated Api object, which
    // relies on route_servers and common_pools being populated, while site-prefix inventory must
    // reflect the complete configured set before the API starts serving it. So if we're configured
    // for listen_only, strictly read them from the database (assuming another instance has populated
    // them), otherwise, populate them now.
    //
    // Pool reconciliation specifically must happen before `create_common_pools` runs below, because
    // that call queries `resource_pool` and bails if any mandatory pool is missing or empty.
    if let Some(seed_data) = seed_data.as_ref() {
        // Determine the authoritative list of resource_pools to seed into the database
        let mut txn = Transaction::begin(&db_pool).await?;
        db::resource_pool::reconcile_pool_defs(&mut txn, &seed_data.initial_pools).await?;

        // We'll always update whatever route servers are in the config
        // to the database, and then leverage the enable_route_servers
        // flag where needed to determine if we actually want to use
        // them (like in api/src/handlers/dpu.rs). This allows us
        // to decouple the configuration from the feature, and control
        // the feature separately (it can get confusing -- and potentially
        // buggy -- otherwise).
        //
        // These are of course set with RouteServerSourceType::ConfigFile.
        db::route_servers::replace(
            &mut txn,
            &carbide_config.route_servers,
            RouteServerSourceType::ConfigFile,
        )
        .await?;

        db::site_prefix::reconcile_configured(&mut txn, &carbide_config.site_fabric_prefixes)
            .await?;

        if !carbide_config.site_fabric_prefixes.is_empty() {
            let lineage =
                db::site_prefix::backfill_vpc_prefix_site_prefix_lineage(&mut txn).await?;
            eyre::ensure!(
                lineage.unresolved_vpc_prefix_count() == 0,
                "VpcPrefix SitePrefix lineage preflight failed: missing VpcPrefix IDs: {:?}; ambiguous VpcPrefixes: {:?}",
                lineage.missing_vpc_prefix_ids,
                lineage.ambiguous,
            );
        }

        txn.commit().await?;

        // Idempotently seed the dedicated site-wide lockdown IKM (v0) from the
        // site-wide BMC root, so existing sites converge onto the decoupled
        // lockdown key without operator action. No-op once seeded or if the BMC
        // root is not yet configured.
        crate::dpa::lockdown::ensure_lockdown_ikm_seeded(&*credential_manager).await?;

        // Initial credential-rotation bookkeeping is backfilled by the
        // `*_credential_rotation_backfill` data migration (see its header for the
        // ordering invariants), not seeded here.
    };

    // A listen-only replica trusts another instance to reconcile configuration,
    // but it still must not serve a configured-root site with unresolved
    // VpcPrefix lineage.
    if carbide_config.listen_only && !carbide_config.site_fabric_prefixes.is_empty() {
        let unassigned =
            db::site_prefix::find_unassigned_vpc_prefix_site_prefix_ids(&db_pool).await?;
        eyre::ensure!(
            unassigned.is_empty(),
            "VpcPrefix SitePrefix lineage preflight failed: unassigned VpcPrefix IDs: {:?}",
            unassigned,
        );
    }

    let common_pools =
        db::resource_pool::create_common_pools(db_pool.clone(), ib_fabric_ids).await?;

    let ib_fabric_manager_impl = ib::create_ib_fabric_manager(
        credential_manager.clone(),
        ib::IBFabricManagerConfig {
            endpoints: if ib_config.enabled {
                carbide_config
                    .ib_fabrics
                    .iter()
                    .map(|(fabric_id, fabric_definition)| {
                        (fabric_id.clone(), fabric_definition.endpoints.clone())
                    })
                    .collect()
            } else {
                Default::default()
            },
            allow_insecure_fabric_configuration: ib_config.allow_insecure,
            manager_type: fabric_manager_type,
            max_partition_per_tenant: ib_config.max_partition_per_tenant,
            mtu: ib_config.mtu,
            rate_limit: ib_config.rate_limit,
            service_level: ib_config.service_level,
            fabric_manager_run_interval: ib_config.fabric_monitor_run_interval,
        },
    )?;

    let ib_fabric_manager: Arc<dyn IBFabricManager> = Arc::new(ib_fabric_manager_impl);

    let site_fabric_prefixes = ethernet_virtualization::SiteFabricPrefixList::from_ipnetwork_vec(
        carbide_config.site_fabric_prefixes.clone(),
    );

    let eth_data = ethernet_virtualization::EthVirtData {
        asn: carbide_config.asn,
        dhcp_servers: carbide_config.dhcp_servers.clone(),
        deny_prefixes: carbide_config.deny_prefixes.clone(),
        site_fabric_prefixes,
    };

    let listen_mode = match &carbide_config.listen_mode {
        ListenMode::Tls => {
            let tls_ref = carbide_config.tls.as_ref().expect("Missing tls config");

            let tls_config = Arc::new(listener::ApiTlsConfig {
                identity_pemfile_path: tls_ref.identity_pemfile_path.clone(),
                identity_keyfile_path: tls_ref.identity_keyfile_path.clone(),
                root_cafile_path: tls_ref.root_cafile_path.clone(),
                admin_root_cafile_path: tls_ref.admin_root_cafile_path.clone(),
            });

            ApiListenMode::Tls(tls_config)
        }
        ListenMode::PlaintextHttp1 => ApiListenMode::PlaintextHttp1,
        ListenMode::PlaintextHttp2 => ApiListenMode::PlaintextHttp2,
    };

    let bmc_session_store: Arc<dyn crate::credentials::BmcSessionStore> =
        Arc::new(crate::credentials::PgBmcSessionStore::new(db_pool.clone()));
    let bmc_session_manager = Arc::new(crate::credentials::BmcSessionManager::new(
        shared_nv_redfish_pool.clone(),
        credential_manager.clone(),
        bmc_session_store,
        carbide_config.bmc_session_lockout_threshold,
        carbide_config.allow_bmc_basic_auth_fallback,
        carbide_config.bmc_max_sessions_per_caller,
    ));

    let bmc_client = Arc::new(AuthenticatedBmcClient::new(
        bmc_credential_ops.clone(),
        shared_nv_redfish_pool,
        ipmi_tool.clone(),
        credential_manager.clone(),
    ));
    let bmc_explorer = carbide_site_explorer::new_bmc_explorer(
        bmc_client.clone(),
        carbide_config
            .site_explorer
            .rotate_switch_nvos_credentials
            .clone(),
        carbide_config.site_explorer.explore_mode,
        db_pool.clone(),
    );
    let endpoint_exploration_service = Arc::new(EndpointExplorationService::new(
        db_pool.clone(),
        bmc_explorer.clone(),
        Arc::new(carbide_config.get_firmware_config()),
    ));

    let nvlink_config = carbide_config.nvlink_config.clone().unwrap_or_default();

    let mut nmxc_builder = libnmxc::NmxcClientPool::builder();
    if let Some(tls) = nmxc_tls_config_from_nvlink(&nvlink_config) {
        nmxc_builder = nmxc_builder.tls(tls);
    }
    let nmxc_client_pool = nmxc_builder
        .build()
        .map_err(|e| eyre::eyre!("failed to build NMX-C client pool: {e}"))?;
    let shared_nmxc_pool: Arc<dyn libnmxc::NmxcPool> = Arc::new(nmxc_client_pool);

    // Node-auth (Scout / DPU-agent bearer JWT, #355) preflight. Run before any
    // DPF resource creation below, so a misconfiguration (the
    // enabled=false + mtls_enabled=false lockout, bearer-over-plaintext, or an
    // unreadable trust anchor) fails startup before it mutates cluster state.
    // Validate unconditionally; when explicitly enabled, missing prerequisites
    // fail rather than silently degrading.
    carbide_config.node_auth.validate()?;
    let node_jwt_validator = if carbide_config.node_auth.enabled {
        // Bearer tokens must never be accepted over plaintext, and the
        // validator trusts the same roots the TLS listener uses for client
        // certificates — so a TLS listener is required on both counts.
        if !matches!(carbide_config.listen_mode, ListenMode::Tls) {
            return Err(eyre::eyre!(
                "[node_auth] is enabled but listen_mode is not \"tls\"; bearer tokens must not be accepted over plaintext"
            ));
        }
        let tls_ref = carbide_config
            .tls
            .as_ref()
            .ok_or_else(|| eyre::eyre!("[node_auth] is enabled but [tls] is unset"))?;
        Some(Arc::new(
            crate::node_auth::NodeJwtValidator::from_root_ca_file(
                &tls_ref.root_cafile_path,
                &carbide_config.node_auth,
            )?,
        ))
    } else {
        None
    };

    let dpf_sdk = initialize_dpf_sdk(
        &carbide_config,
        credential_manager.clone(),
        db_pool.clone(),
        join_set,
    )
    .await?;

    let component_manager = if let Some(cd_config) = &carbide_config.component_manager {
        match component_manager::component_manager::build_component_manager(
            cd_config,
            carbide_config.rack_profiles.clone(),
            rms_client.clone(),
            switch_system_image_rms_api.clone().map(|client| {
                client as Arc<dyn component_manager::rms::RmsSwitchSystemImageStatusApi>
            }),
            Some(db_pool.clone()),
            Some(shared_redfish_pool.clone()),
        )
        .await
        {
            Ok(cm) => {
                tracing::info!(
                    nv_switch_backend = cm.nv_switch.name(),
                    power_shelf_backend = cm.power_shelf.name(),
                    compute_tray_backend = cm.compute_tray.name(),
                    "Component manager configured",
                );
                Some(cm)
            }
            Err(e) => {
                // The nv-switch, power-shelf, and compute-tray backends are
                // currently required fields, so they are initialized all-or-
                // nothing: if any one backend fails to build (for example,
                // compute_tray_backend defaults to 'rms' but no RMS client is
                // configured), the other two are discarded as well and the
                // entire component manager is left uninitialized. All component
                // manager RPCs (switch, power-shelf, and compute-tray) will be
                // unavailable until the [component_manager] config is fixed.
                // TODO: make the three backends individually optional so a bad
                // config for one backend does not disable the others.
                tracing::error!(
                    error = %e,
                    "Component manager NOT initialized; failed to build one of the nv-switch / power-shelf / compute-tray backends",
                );
                None
            }
        }
    } else {
        tracing::info!(
            "No [component_manager] config found; component manager RPCs will be unavailable"
        );
        None
    };

    let api_service = Arc::new(Api {
        certificate_provider,
        common_pools,
        credential_manager,
        node_jwt_validator,
        database_connection: db_pool.clone(),
        dpu_health_log_limiter: LogLimiter::default(),
        dynamic_settings,
        endpoint_explorer: bmc_explorer,
        bmc_client,
        endpoint_exploration_service: endpoint_exploration_service.clone(),
        eth_data,
        ib_fabric_manager,
        redfish_pool: shared_redfish_pool,
        bmc_credential_ops: bmc_credential_ops.clone(),
        bmc_session_manager,
        runtime_config: carbide_config.clone(),
        scout_stream_registry: ConnectionRegistry::new(),
        rms_client: rms_client.clone(),
        nmxc_client_pool: shared_nmxc_pool.clone(),
        work_lock_manager_handle,
        dpf_sdk: dpf_sdk.clone(),
        machine_state_handler_enqueuer: Enqueuer::new(db_pool),
        metric_emitter: ApiMetricsEmitter::new(&meter),
        component_manager,
        bms_client: std::sync::OnceLock::new(),
        secrets_context,
    });

    if carbide_config.listen_only {
        tracing::info!("Not starting background services, as listen_only=true");
    } else {
        initialize_and_start_controllers(
            join_set,
            api_service.clone(),
            meter.clone(),
            per_object_prometheus_registry,
            ipmi_tool.clone(),
            seed_data,
            cancel_token.clone(),
        )
        .await?;
    };

    // Honor the `enable_admin_ui` config flag (default true): when disabled, drop
    // the admin UI routes builder so the listener serves only the gRPC API. The
    // top-level binary always supplies the builder; the decision to use it lives
    // here, next to the parsed config.
    let admin_ui_routes_builder = if carbide_config.enable_admin_ui {
        admin_ui_routes_builder
    } else {
        tracing::info!("admin web UI disabled via enable_admin_ui=false");
        None
    };

    let listen_address = listener::start(
        join_set,
        api_service,
        listen_mode,
        carbide_config.listen,
        &carbide_config.auth,
        meter,
        admin_ui_routes_builder,
        cancel_token.clone(),
    )
    .await?;

    Ok(listen_address)
}

/// Normalizes and validates DPF-only intercept-bridging topology without retaining legacy map keys.
fn normalize_dpf_intercept_bridging(
    config: Option<&VmaasConfig>,
    num_of_vfs: u32,
) -> eyre::Result<Option<carbide_dpf::DpfInterceptBridging>> {
    // Only complete VMaaS absence selects static inventory; present invalid maps fail below.
    let Some(config) = config else {
        return Ok(None);
    };
    let entries = config
        .bridging
        .as_ref()
        .map(|bridging| &bridging.host_representor_intercept_bridging);
    let interfaces = entries
        .into_iter()
        .flatten()
        // Legacy keys do not affect DPF output; sorting only stabilizes which invalid entry is
        // reported first when multiple entries fail validation.
        .sorted_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(legacy_key, interface)| {
            eyre::ensure!(
                !interface.skip_create,
                "DPF intercept-bridging interface {legacy_key:?} cannot use skip_create=true"
            );
            let identity = interface.dpf_interface.ok_or_else(|| {
                eyre::eyre!(
                    "DPF intercept-bridging interface {legacy_key:?} is missing dpf_interface"
                )
            })?;
            Ok(carbide_dpf::DpfInterceptBridge::new(
                carbide_dpf::DpfInterfaceIdentity {
                    controller_id: identity.controller_id,
                    pf_id: identity.pf_id,
                    vf_id: identity.vf_id,
                },
                &interface.bridge,
                &interface.patch_port,
            ))
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    // DPF-local validation owns typed identity and all rendered-name constraints.
    carbide_dpf::DpfInterceptBridging::new(interfaces, num_of_vfs)
        .map(Some)
        .map_err(|error| eyre::eyre!("invalid DPF intercept-bridging configuration: {error}"))
}

/// Initialize the DPF SDK and create all required Kubernetes CRs.
///
/// Returns `None` (with a deprecation warning) when DPF is disabled.
async fn initialize_dpf_sdk(
    carbide_config: &CarbideConfig,
    credential_manager: Arc<dyn CredentialManager>,
    db_pool: PgPool,
    join_set: &mut JoinSet<()>,
) -> eyre::Result<Option<Arc<dyn DpfOperations>>> {
    // Astra is a BF4+CX9-only deployment with a distinct interface inventory.
    // Reject unsafe global ServiceInterfaces even when DPF is disabled so a dormant Astra
    // configuration cannot become unsafe merely by enabling DPF later.
    carbide_config.dpf.validate_service_interface_scoping()?;

    if !carbide_config.dpf.enabled {
        tracing::warn!(
            docs = "https://docs.nvidia.com/infra-controller/documentation/getting-started/installation-options/dpf-setup",
            "iPXE provisioning strategy (internally) is deprecated; enable DPF management for DPUs to migrate"
        );
        return Ok(None);
    }

    let mut deployments = vec!["bf3", "bf3_gb200"];
    if carbide_config.dpf.deployments.bf4_generic.is_some() {
        deployments.push("bf4_generic");
    }
    if carbide_config.dpf.deployments.bf4_astra.is_some() {
        deployments.push("bf4_astra");
    }
    tracing::info!(?deployments, "Initializing DPF SDK");

    carbide_config
        .dpf
        .dpu_agent_bootstrap_ca
        .validate()
        .map_err(|err| eyre::eyre!("invalid DPF bootstrap CA configuration: {err}"))?;

    // Validate the complete site topology before constructing a repository or writing any CR.
    let intercept_bridging = normalize_dpf_intercept_bridging(
        carbide_config.vmaas_config.as_ref(),
        carbide_config.dpu_config.num_of_vfs,
    )?;
    let effective_interfaces = carbide_dpf::build_effective_dpu_interfaces(
        carbide_config.dpu_config.num_of_vfs,
        intercept_bridging.as_ref(),
    );

    let astra_interfaces = carbide_dpf::sdk::build_dpu_interfaces_vec();

    // SDK construction writes the shared BMC Secret, so capacity validation must remain on the
    // pure configuration path and finish before Kubernetes repository construction.
    let service_vpc_interfaces = crate::dpf_services::service_vpc_interfaces(
        &effective_interfaces,
        carbide_config.dpu_config.service_vpc_slot_count,
    )
    .map_err(|error| eyre::eyre!("invalid DPF HBN interface configuration: {error}"))?;
    let additional_managed_sf = carbide_config
        .dpu_config
        .service_vpc_slot_count
        .checked_add(carbide_config.dpu_config.additional_managed_sf)
        .ok_or_else(|| eyre::eyre!("dpu_config managed SF count exceeds u32"))?;
    carbide_dpf::calculate_pf_total_sf(
        &effective_interfaces,
        intercept_bridging.as_ref(),
        carbide_config.dpf.pf_total_sf_reserved,
        additional_managed_sf,
    )
    .map_err(|error| eyre::eyre!("invalid DPF SF configuration: {error}"))?;

    let repo = carbide_dpf::KubeRepository::new()
        .await
        .map_err(|e| eyre::eyre!("failed to create DPF repository: {e}"))?;

    let provider = CarbideBmcPasswordProvider::new(credential_manager, db_pool.clone());

    carbide_config
        .dpf
        .deployments
        .validate_unique_identifiers()
        .map_err(|err| eyre::eyre!("invalid DPF deployment configuration: {err}"))?;

    carbide_config
        .dpf
        .deployments
        .validate_provisioning_sources()
        .map_err(|err| eyre::eyre!("invalid DPF deployment configuration: {err}"))?;

    // This is just temporary code until we make v2 only option. (just 2 weeks)
    // Soon v2 flag will be removed and will become only mode for dpf handling.
    let deployment_type_labels = build_deployment_type_labels(carbide_config);

    let sdk = carbide_dpf::DpfSdkBuilder::new(repo, carbide_dpf::NAMESPACE, provider)
        .with_labeler(
            CarbideDPFLabeler::new(carbide_config.dpf.deployments.bf3.node_label_key.clone())
                .with_deployment_type_labels(deployment_type_labels),
        )
        .with_bmc_password_refresh_interval(std::time::Duration::from_secs(60))
        .with_join_set(join_set)
        .build_without_resources()
        .await
        .map_err(|err| eyre::eyre!("failed to initialize DPF SDK: {err}"))?;

    // Builds the SDK init config for one DPUDeployment. BF4 uses a single
    // `BlueFieldSoftware` source (the CR itself carries the PSID→PLDM mapping);
    // config validation guarantees exactly one PSID entry.
    let make_init_config =
        |deployment: &crate::cfg::file::DpfDeploymentConfig,
         deployment_type: DpuDeploymentType,
         bluefield_software: Option<carbide_dpf::BlueFieldSoftwareParams>| {
            let services = carbide_config
                .dpf
                .resolved_services_for(deployment, deployment_type);
            let interfaces = match deployment_type {
                DpuDeploymentType::Bf4Astra => &astra_interfaces,
                DpuDeploymentType::Bf3
                | DpuDeploymentType::Bf3Gb200
                | DpuDeploymentType::Bf4Generic => &effective_interfaces,
            };
            let (service_vpc_interfaces, additional_managed_sf) = match deployment_type {
                DpuDeploymentType::Bf4Astra => (&[][..], 0),
                DpuDeploymentType::Bf3
                | DpuDeploymentType::Bf3Gb200
                | DpuDeploymentType::Bf4Generic => {
                    (service_vpc_interfaces.as_slice(), additional_managed_sf)
                }
            };
            carbide_dpf::InitDpfResourcesConfig {
                bfb_url: deployment.bfb_url.clone().unwrap_or_default(),
                bluefield_software,
                flavor_name: deployment.flavor_name.clone(),
                deployment_name: deployment.deployment_name.clone(),
                deployment_scoped_service_interfaces: carbide_config
                    .dpf
                    .deployment_scoped_service_interfaces,
                services: crate::dpf_services::mandatory_services(
                    &services,
                    &carbide_config.dpf.dpu_agent_bootstrap_ca,
                    interfaces,
                    service_vpc_interfaces,
                    &carbide_config.node_auth,
                ),
                num_of_vfs: carbide_config.dpu_config.num_of_vfs,
                pf_total_sf_reserved: carbide_config.dpf.pf_total_sf_reserved,
                additional_managed_sf,
                intercept_bridging: match deployment_type {
                    DpuDeploymentType::Bf4Astra => None,
                    DpuDeploymentType::Bf3
                    | DpuDeploymentType::Bf3Gb200
                    | DpuDeploymentType::Bf4Generic => intercept_bridging.clone(),
                },
                interfaces: interfaces.clone(),
                proxy: carbide_config.dpf.proxy.clone(),
                extra_bfcfg_parameters: carbide_config
                    .dpf
                    .resolved_bfcfg_parameters_for(deployment),
                deployment_type,
            }
        };

    let bf3 = &carbide_config.dpf.deployments.bf3;
    sdk.create_initialization_objects(&make_init_config(bf3, DpuDeploymentType::Bf3, None))
        .await
        .map_err(|err| eyre::eyre!("failed to initialize bf3 DPF deployment: {err}"))?;

    let bf3_gb200 = bf3.bf3_gb200();
    sdk.create_initialization_objects(&make_init_config(
        &bf3_gb200,
        DpuDeploymentType::Bf3Gb200,
        None,
    ))
    .await
    .map_err(|err| eyre::eyre!("failed to initialize bf3 GB200 DPF deployment: {err}"))?;

    if let Some(bf4) = &carbide_config.dpf.deployments.bf4_generic {
        // Validation guarantees `bluefield_software` is set with exactly one PSID
        // entry for a BF4 deployment.
        let bfs = bf4.bluefield_software.as_ref().ok_or_else(|| {
            eyre::eyre!("bf4_generic DPF deployment is missing bluefield_software")
        })?;
        let pldm_url =
            bfs.pldm_fw_bundle.values().next().ok_or_else(|| {
                eyre::eyre!("bf4_generic DPF deployment has an empty pldm_fw_bundle")
            })?;
        let params = carbide_dpf::BlueFieldSoftwareParams {
            os_iso: bfs.os_iso.clone(),
            pldm_fw_bundle: Some(pldm_url.clone()),
        };
        sdk.create_initialization_objects(&make_init_config(
            bf4,
            DpuDeploymentType::Bf4Generic,
            Some(params),
        ))
        .await
        .map_err(|err| eyre::eyre!("failed to initialize bf4_generic DPF deployment: {err}"))?;
    }

    if let Some(bf4_astra) = &carbide_config.dpf.deployments.bf4_astra {
        let bfs = bf4_astra
            .bluefield_software
            .as_ref()
            .ok_or_else(|| eyre::eyre!("bf4_astra DPF deployment is missing bluefield_software"))?;
        let pldm_url =
            bfs.pldm_fw_bundle.values().next().ok_or_else(|| {
                eyre::eyre!("bf4_astra DPF deployment has an empty pldm_fw_bundle")
            })?;
        let params = carbide_dpf::BlueFieldSoftwareParams {
            os_iso: bfs.os_iso.clone(),
            pldm_fw_bundle: Some(pldm_url.clone()),
        };
        sdk.create_initialization_objects(&make_init_config(
            bf4_astra,
            DpuDeploymentType::Bf4Astra,
            Some(params),
        ))
        .await
        .map_err(|err| eyre::eyre!("failed to initialize bf4_astra DPF deployment: {err}"))?;
    }

    Ok(Some(Arc::new(DpfSdkOps::new(
        Arc::new(sdk),
        db_pool,
        join_set,
    )?)))
}

/// Build per-deployment-type node selector labels for the DPF labeler registry.
///
/// Each deployment gets two labels: the shared `dpu-enabled` marker and its
/// own deployment-specific key. Both BF3 variants are always included;
/// configured BF4 variants are added.
fn build_deployment_type_labels(
    carbide_config: &CarbideConfig,
) -> std::collections::BTreeMap<DpuDeploymentType, std::collections::BTreeMap<String, String>> {
    let make_labels = |key: &str| {
        std::collections::BTreeMap::from([
            (
                carbide_dpf::DPU_ENABLED_NODE_LABEL.to_string(),
                "true".to_string(),
            ),
            (key.to_string(), "true".to_string()),
        ])
    };

    let mut map = std::collections::BTreeMap::from([(
        DpuDeploymentType::Bf3,
        make_labels(&carbide_config.dpf.deployments.bf3.node_label_key),
    )]);
    let bf3_gb200 = carbide_config.dpf.deployments.bf3.bf3_gb200();
    map.insert(
        DpuDeploymentType::Bf3Gb200,
        make_labels(&bf3_gb200.node_label_key),
    );

    if let Some(bf4) = &carbide_config.dpf.deployments.bf4_generic {
        map.insert(
            DpuDeploymentType::Bf4Generic,
            make_labels(&bf4.node_label_key),
        );
    }

    if let Some(bf4_astra) = &carbide_config.dpf.deployments.bf4_astra {
        map.insert(
            DpuDeploymentType::Bf4Astra,
            make_labels(&bf4_astra.node_label_key),
        );
    }

    map
}

#[derive(Debug)]
struct SeedData<'a> {
    initial_networks: Cow<'a, HashMap<String, NetworkDefinition>>,
    initial_vpcs: Cow<'a, HashMap<String, VpcDefinition>>,
    initial_pools: Cow<'a, HashMap<String, ResourcePoolDef>>,
}

trait SeedKind: Clone + PartialEq {
    fn name() -> &'static str;
    fn source_description(cfg: &CarbideConfig, name: &str) -> String;
}

impl SeedKind for NetworkDefinition {
    fn name() -> &'static str {
        "Network"
    }

    fn source_description(cfg: &CarbideConfig, name: &str) -> String {
        cfg.config_ctx
            .as_ref()
            .and_then(|f| f.find_metadata(&format!("networks.{name}")))
            .and_then(|m| m.source.as_ref())
            .map(|source| source.to_string())
            .unwrap_or_else(|| "carbide-api config".to_string())
    }
}

impl SeedKind for VpcDefinition {
    fn name() -> &'static str {
        "VPC"
    }

    fn source_description(cfg: &CarbideConfig, name: &str) -> String {
        cfg.config_ctx
            .as_ref()
            .and_then(|f| f.find_metadata(&format!("vpcs.{name}")))
            .and_then(|m| m.source.as_ref())
            .map(|source| source.to_string())
            .unwrap_or_else(|| "carbide-api config".to_string())
    }
}

impl SeedKind for ResourcePoolDef {
    fn name() -> &'static str {
        "Resource pool"
    }

    fn source_description(cfg: &CarbideConfig, name: &str) -> String {
        cfg.config_ctx
            .as_ref()
            .and_then(|f| f.find_metadata(&format!("pools.{name}")))
            .and_then(|m| m.source.as_ref())
            .map(|source| source.to_string())
            .unwrap_or_else(|| "carbide-api config".to_string())
    }
}

impl<'a> SeedData<'a> {
    /// Determines the authoritative set of seed data definitions to reconcile
    /// against the database at startup, merging e.g. `InitialObjectsConfig.networks`
    /// with the legacy `CarbideConfig.networks` source.
    fn resolve(
        carbide_config: &'a CarbideConfig,
        initial_objects: Option<&'a InitialObjectsConfig>,
    ) -> eyre::Result<Self> {
        let initial_networks = Self::merge_objects(
            initial_objects.and_then(|io| io.networks.as_ref()),
            carbide_config.networks.as_ref(),
            carbide_config,
            false,
        )?;

        for (name, defn) in initial_networks.iter() {
            defn.validate(name).map_err(eyre::Report::from)?;
        }

        let initial_vpcs = Self::merge_objects(
            initial_objects.and_then(|io| io.vpcs.as_ref()),
            carbide_config.vpcs.as_ref(),
            carbide_config,
            false,
        )?;
        db_init::validate_initial_vpcs(&initial_vpcs)?;

        let initial_pools = Self::merge_objects(
            initial_objects.and_then(|io| io.pools.as_ref()),
            carbide_config.pools.as_ref(),
            carbide_config,
            true,
        )?;

        Ok(Self {
            initial_networks,
            initial_vpcs,
            initial_pools,
        })
    }

    fn merge_objects<T: SeedKind>(
        from_initial_objects: Option<&'a HashMap<String, T>>,
        from_carbide_config: Option<&'a HashMap<String, T>>,
        carbide_config: &CarbideConfig,
        required: bool,
    ) -> eyre::Result<Cow<'a, HashMap<String, T>>> {
        let kind = T::name();

        match (from_initial_objects, from_carbide_config) {
            // No objects are defined anywhere — raise an error
            (None, None) if required => Err(DefineResourcePoolError::InvalidArgument(format!(
                "No {kind}s are defined in loaded configuration files: {:?}",
                all_configuration_files(carbide_config)
            ))
            .into()),
            // No objects are defined anywhere — initial creation is skipped.
            (None, None) => Ok(Cow::Owned(HashMap::new())),
            // Objects are defined in InitialObjectsConfig.networks
            (Some(io), None) => Ok(Cow::Borrowed(io)),
            // Objects are defined only in the legacy CarbideConfig.networks
            (None, Some(cc)) => {
                for name in cc.keys() {
                    let source = T::source_description(carbide_config, name);
                    tracing::warn!(
                        object_kind = %kind,
                        object_name = %name,
                        source = %source,
                        "Initial object is defined in a deprecated configuration source; move the definition into `initial_objects_file`.",
                    );
                }
                Ok(Cow::Borrowed(cc))
            }
            // Objects are defined in both sources.
            (Some(io), Some(cc)) => {
                // detect conflicts.
                let conflicts: Vec<&str> = cc
                    .iter()
                    .filter(|(name, legacy_def)| {
                        io.get(name.as_str())
                            .is_some_and(|new_def| new_def != *legacy_def)
                    })
                    .map(|(name, _)| name.as_str())
                    .collect();

                if !conflicts.is_empty() {
                    // Each conflicting name is declared in both sources.
                    // Name them both so the operator knows which two files
                    // to compare.
                    let conflict_details: Vec<String> = conflicts
                        .iter()
                        .map(|name| {
                            format!(
                                "`{name}` (in initial_objects_file vs {})",
                                T::source_description(carbide_config, name),
                            )
                        })
                        .collect();
                    return Err(eyre::eyre!(
                        "{kind} has conflicting definitions {conflict_details:?}. \
                         reconcile each object by removing it from one source",
                    ));
                }

                // merge legacy-only entries into the result.
                let mut merged = Cow::Borrowed(io);
                for (name, legacy_def) in cc {
                    if !io.contains_key(name) {
                        merged.to_mut().insert(name.clone(), legacy_def.clone());
                    }
                }

                // Every name in `cc` is still in the deprecated source —
                // emit one warning per name regardless of whether it was a
                // legacy-only entry or an identical overlap.
                for name in cc.keys() {
                    let source = T::source_description(carbide_config, name);
                    tracing::warn!(
                        object_kind = %kind,
                        object_name = %name,
                        source = %source,
                        "Initial object is still defined in a deprecated configuration source; move it into `initial_objects_file`.",
                    );
                }
                Ok(merged)
            }
        }
    }
}

/// Initialize and spawn all controllers and background tasks.
///
/// All background tasks will be spawned into `join_set`, which can be awaited with
/// [`JoinSet::join_all`] to wait for them to complete.
async fn initialize_and_start_controllers<'a>(
    join_set: &mut JoinSet<()>,
    api_service: Arc<Api>,
    meter: Meter,
    per_object_prometheus_registry: Option<prometheus::Registry>,
    ipmi_tool: Arc<dyn IPMITool>,
    seed_data: Option<SeedData<'a>>,
    cancel_token: CancellationToken,
) -> eyre::Result<()> {
    let Api {
        runtime_config: carbide_config,
        endpoint_exploration_service,
        common_pools,
        database_connection: db_pool,
        ib_fabric_manager,
        redfish_pool: shared_redfish_pool,
        bmc_credential_ops,
        work_lock_manager_handle,
        rms_client,
        component_manager,
        dpf_sdk,
        credential_manager,
        ..
    } = api_service.as_ref();

    let nvos_update_manager = rms_client.clone().map(|client| {
        Arc::new(component_manager::rms::rms_nvos_update_manager(client))
            as Arc<dyn component_manager::NvosUpdateManager>
    });

    // As soon as we get the database up, observe this version of forge so that we know when it was
    // first deployed
    {
        let mut txn = Transaction::begin(db_pool).await?;

        db::carbide_version::observe_as_latest_version(
            &mut txn,
            carbide_version::v!(build_version),
        )
        .await?;

        txn.commit().await?;
    }

    if let Some(domain_name) = &carbide_config.initial_domain_name
        && db_init::create_initial_domain(db_pool.clone(), domain_name).await?
    {
        tracing::info!(
            domain_name = %domain_name,
            "Created initial domain",
        );
    }

    // Probe the helm-chart layout first, then the forged-kustomize layout.
    // The first path that exists wins; if reading that path then fails
    // (e.g. permissions) the error propagates rather than silently falling
    // through to the next layout.
    const EXPECTED_MACHINE_FILE_PATHS: &[&str] = &[
        "/etc/nico/nico-api/site/expected_machines.json",
        "/etc/forge/carbide-api/site/expected_machines.json",
    ];
    let expected_machine_path = EXPECTED_MACHINE_FILE_PATHS
        .iter()
        .find(|p| std::path::Path::new(p).exists());
    if let Some(path_used) = expected_machine_path {
        tracing::debug!(path = path_used, "Loading expected_machines.json");
        let file_str = tokio::fs::read_to_string(path_used)
            .await
            .wrap_err_with(|| format!("failed to read {path_used}"))?;
        let expected_machines = serde_json::from_str::<Vec<ExpectedMachine>>(file_str.as_str()).inspect_err(|err| {
                tracing::error!(
                    error = %err,
                    "expected_machines.json file exists, but unable to parse expected_machines file, nothing was written to db, bailing.",
                );
            })?;
        let mut txn = Transaction::begin(db_pool).await?;
        crate::handlers::expected_machine::create_missing_from(&mut txn, &expected_machines)
            .await
            .inspect_err(|err| {
                tracing::error!(
                    error = %err,
                    "Unable to update database from expected_machines list, bailing",
                );
            })?;
        txn.commit().await?;
        tracing::info!("Successfully wrote expected machines to db, continuing startup.");
    } else {
        tracing::info!("No expected machine file found, continuing startup.");
    }

    let ib_config = carbide_config.ib_config.clone().unwrap_or_default();

    if ib_config.enabled {
        // These are some sanity checks until full multi-fabric support is available
        // Right now there is only one fabric supported, and it needs to be called `default`
        if carbide_config.ib_fabrics.len() > 1 {
            return Err(eyre::eyre!(
                "only a single IB fabric definition is allowed at the moment"
            ));
        }

        if !carbide_config.ib_fabrics.is_empty() {
            let fabric_id = carbide_config.ib_fabrics.iter().next().unwrap().0;
            if fabric_id != DEFAULT_IB_FABRIC_NAME {
                return Err(eyre::eyre!(
                    "ib_fabrics contains an entry \"{fabric_id}\", but only \"{DEFAULT_IB_FABRIC_NAME}\" is supported at the moment"
                ));
            }
        }

        // Populate IB specific resource pools
        let mut txn = Transaction::begin(db_pool).await?;

        for (fabric_id, x) in carbide_config.ib_fabrics.iter() {
            db::resource_pool::define(
                &mut txn,
                &model::resource_pool::common::ib_pkey_pool_name(fabric_id),
                &resource_pool::ResourcePoolDef {
                    pool_type: model::resource_pool::define::ResourcePoolType::Integer,
                    ranges: x.pkeys.clone(),
                    prefix: None,
                    delegate_prefix_len: None,
                },
            )
            .await?;
        }

        txn.commit().await?;
    }

    let health_pool = db_pool.clone();
    start_export_service_health_metrics(ServiceHealthContext {
        meter: meter.clone(),
        database_pool: health_pool,
        resource_pool_stats: common_pools.pool_stats.clone(),
    });

    if let Some(seed_data) = seed_data {
        if !seed_data.initial_vpcs.is_empty() {
            db_init::create_initial_vpcs(
                db_pool,
                &seed_data.initial_vpcs,
                common_pools.ethernet.pool_vpc_vni.as_ref(),
            )
            .await?;
        }

        if !seed_data.initial_networks.is_empty() {
            db_init::create_initial_networks(&api_service, db_pool, &seed_data.initial_networks)
                .await?;
        }
    }

    if let Some(fnn_config) = carbide_config.fnn.as_ref()
        && let Some(admin) = fnn_config.admin_vpc.as_ref()
        && admin.enabled
    {
        db_init::create_admin_vpc(db_pool, admin.vpc_vni).await?;
    }
    // Update SVI IP to segments which have VPC attached and type is FNN.
    db_init::update_network_segments_svi_ip(db_pool).await?;

    db_init::store_initial_dpu_agent_upgrade_policy(
        db_pool,
        carbide_config.initial_dpu_agent_upgrade_policy,
    )
    .await?;

    if let Err(error) = update_dpu_loopback_ips_v6(db_pool, common_pools).await {
        tracing::error!(
            error = %error,
            "Failed to update IPv6 loopback IPs for DPUs",
        );
    }

    if let Err(e) = update_dpu_asns(db_pool, common_pools).await {
        tracing::warn!(
            error = %e,
            "Failed to update ASN for DPUs",
        );
    }

    let downloader = FirmwareDownloader::new();
    let upload_limiter = Arc::new(Semaphore::new(carbide_config.firmware_global.max_uploads));

    // Create state change emitter with DSX Exchange Event Bus hook if enabled
    let state_change_emitter = {
        let mut emitter_builder = StateChangeEmitterBuilder::default();

        if let Some(ref config) = carbide_config.dsx_exchange_event_bus
            && config.enabled
        {
            let options = {
                let defaults =
                    mqttea::client::ClientOptions::default().with_qos(mqttea::QoS::AtMostOnce);

                if let Some(provider) = crate::auth::mqtt_auth::build_credentials_provider(
                    &config.auth,
                    carbide_secrets::credentials::CredentialKey::MqttAuth {
                        credential_type:
                            carbide_secrets::credentials::MqttCredentialType::DsxExchangeEventBus,
                    },
                    api_service.credential_manager.clone(),
                )
                .await?
                {
                    defaults.with_credentials_provider(provider)
                } else {
                    defaults
                }
            };

            // Suffix the broker-level client identifier so multiple replicas
            // (or a new pod coming up while the old one is still terminating)
            // do not race for the same MQTT session and ping-pong each other
            // off the broker.
            let client_id = mqttea::unique_client_id("carbide-dsx-exchange-event-bus");
            let client = mqttea::MqtteaClient::new(
                &config.mqtt_endpoint,
                config.mqtt_broker_port,
                &client_id,
                Some(options),
            )
            .map_err(|e| eyre::eyre!("failed to create DSX exchange event bus MQTT client: {e}"))
            .await?;

            client.connect().await.map_err(|e| {
                eyre::eyre!("failed to connect DSX exchange event bus MQTT client: {e}")
            })?;
            client.register_metrics(&meter, "dsx_event_bus");

            tracing::info!(
                mqtt_endpoint = %config.mqtt_endpoint,
                mqtt_broker_port = config.mqtt_broker_port,
                "DSX Exchange Event Bus enabled",
            );

            let bms_client = BmsDsxExchangeHandle::new(
                client.clone(),
                db_pool,
                join_set,
                config.publish_timeout,
                config.queue_capacity,
                &meter,
                cancel_token.clone(),
            )
            .await?;

            api_service
                .bms_client
                .set(bms_client)
                .map_err(|_| eyre::eyre!("BMS DSX exchange handle already initialized"))?;

            // Periodically re-publish current managed host state so consumers
            // that miss change events can reconcile. A no-op unless enabled.
            ManagedHostStateRepublisher::new(
                client.clone(),
                ManagedHostStateRepublisherParams {
                    db_pool: db_pool.clone(),
                    work_lock_manager_handle: work_lock_manager_handle.clone(),
                    topic_prefix: config.topic_prefix.clone(),
                    publish_timeout: config.publish_timeout,
                    config: config.periodic_state_republish.clone(),
                    host_health_config: carbide_config.host_health,
                },
            )
            .start(join_set, cancel_token.clone())?;

            emitter_builder = emitter_builder.hook(Box::new(MqttStateChangeHook::new(
                client,
                join_set,
                config.publish_timeout,
                config.topic_prefix.clone(),
                config.queue_capacity,
                &meter,
                cancel_token.clone(),
            )));
        }

        emitter_builder.build()
    };

    // Use the hostname as cluster-wide state controller ID
    // The expectation here is that either the host only runs a single
    // carbide instance natively, or - if the multiple instances run as containers
    // - every container gets its own hostname (k8s pod name)
    let state_controller_id = hostname::get()
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string().into())
        .to_string_lossy()
        .to_string();

    // Every controller that records per-object series, in one place so the
    // two hold-period computations below cannot drift apart. The first four
    // also feed the per-object health classification metric.
    let per_object_feeding_controllers = [
        &carbide_config.machine_state_controller.controller,
        &carbide_config.switch_state_controller.controller,
        &carbide_config.rack_state_controller.controller,
        &carbide_config.power_shelf_state_controller.controller,
        &carbide_config.network_segment_state_controller.controller,
        &carbide_config.vpc_prefix_state_controller.controller,
        &carbide_config.extension_service_state_controller.controller,
        &carbide_config.spdm_state_controller.controller,
        &carbide_config.ib_partition_state_controller.controller,
    ];

    // Cross-controller registry feeding the per-object health and state
    // metrics; shared by every state controller and registered once. The
    // classification gauge keeps its own hold derived only from the four
    // controllers that record it, so tuning an unrelated controller's cadence
    // cannot inflate eviction of the pre-existing main-endpoint metric.
    let classification_hold_time = per_object_feeding_controllers[..4]
        .iter()
        .map(|controller| controller.metric_hold_time)
        .max()
        .unwrap_or_default();
    let per_object_metrics_registry = PerObjectMetricsRegistry::new(
        carbide_config
            .observability
            .per_object_metrics_for_classifications
            .clone(),
        classification_hold_time.saturating_add(std::time::Duration::from_secs(60)),
    );
    per_object_metrics_registry.register(&meter);

    // Hold period for the per-object state/info gauges, which are fed by all
    // eight controllers. An object's series is only refreshed after its
    // handler finishes, so the worst-case refresh gap is roughly an iteration
    // interval (with slack) PLUS the longest allowed handler runtime. Note:
    // under fleet backlog the wall-clock iteration can exceed iteration_time;
    // the hold cannot bound that statically.
    let per_object_state_hold_time = per_object_feeding_controllers
        .iter()
        .map(|controller| {
            controller.metric_hold_time.max(
                controller.iteration_time
                    + controller.iteration_time / 3
                    + controller.max_object_handling_time,
            )
        })
        .max()
        .unwrap_or_default()
        .saturating_add(std::time::Duration::from_secs(60));

    // Per-object state progress metrics (opt-in): the state gauges are fed by
    // the generic processors as native collectors on the dedicated per-object
    // Prometheus registry, so they are served from their own endpoint. Object
    // types are filtered via `observability.per_object_state_metrics
    // .object_types` (a config enum, so unknown tokens fail deserialization).
    let per_object_state_metrics = per_object_prometheus_registry
        .as_ref()
        .map(|prometheus_registry| {
            PerObjectStateMetrics::new(
                &per_object_metrics_registry,
                prometheus_registry,
                per_object_state_hold_time,
            )
        })
        .transpose()?;
    let per_object_state_recorder = |object_type: &'static str| -> Option<PerObjectStateRecorder> {
        let metrics = per_object_state_metrics.clone()?;
        carbide_config
            .observability
            .per_object_state_metrics
            .object_types
            .iter()
            .any(|t| t.as_str() == object_type)
            .then_some(PerObjectStateRecorder::new(object_type, metrics))
    };
    // Machine trait/association info series accompany the machine state
    // series, so both are gated by the same recorder.
    let machine_state_recorder = per_object_state_recorder("machine");
    let machine_per_object_info = per_object_prometheus_registry
        .as_ref()
        .filter(|_| machine_state_recorder.is_some())
        .map(|prometheus_registry| {
            MachinePerObjectInfo::new(
                &per_object_metrics_registry,
                prometheus_registry,
                per_object_state_hold_time,
            )
        })
        .transpose()?;

    // handles need to be stored in a variable
    // If they are assigned to _ then the destructor will be immediately called
    StateController::<MachineStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_machines", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            MachineStateHandlerServices {
                db_pool: db_pool.clone(),
                db_reader: db_pool.clone().into(),
                redfish_client_pool: shared_redfish_pool.clone(),
                bmc_credential_ops: bmc_credential_ops.clone(),
                ipmi_tool: ipmi_tool.clone(),
                site_config: carbide_config.machine_state_handler_site_config().into(),
                component_manager: component_manager.clone().map(Arc::new),
                credential_manager: credential_manager.clone(),
                bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::Bmc,
                ),
                host_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::HostUefi,
                ),
                dpu_uefi_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::DpuUefi,
                ),
                dpu_bmc_service_rotation_gate:
                    carbide_credential_rotation::RotationGate::new_for_family(
                        db::credential_rotation::CredentialRotationType::DpuBmcService,
                    ),
                nic_lockdown_rotation_gate:
                    carbide_credential_rotation::RotationGate::new_for_family(
                        db::credential_rotation::CredentialRotationType::LockdownIkm,
                    ),
                per_object_metrics_registry: per_object_metrics_registry.clone(),
                per_object_info: machine_per_object_info,
            }
            .into(),
        )
        .per_object_state_metrics(machine_state_recorder)
        .iteration_config((&carbide_config.machine_state_controller.controller).into())
        .state_handler(Arc::new(
            MachineStateHandlerBuilder::builder()
                .dpu_up_threshold(carbide_config.machine_state_controller.dpu_up_threshold)
                .dpu_nic_firmware_reprovision_update_enabled(
                    carbide_config
                        .dpu_config
                        .dpu_nic_firmware_reprovision_update_enabled,
                )
                .dpu_enable_secure_boot(carbide_config.dpu_config.dpu_enable_secure_boot)
                .dpu_wait_time(carbide_config.machine_state_controller.dpu_wait_time)
                .power_down_wait(carbide_config.machine_state_controller.power_down_wait)
                .failure_retry_time(carbide_config.machine_state_controller.failure_retry_time)
                .scout_reporting_timeout(
                    carbide_config
                        .machine_state_controller
                        .scout_reporting_timeout,
                )
                .waiting_for_measurements_timeout(
                    carbide_config
                        .machine_state_controller
                        .waiting_for_measurements_timeout,
                )
                .uefi_boot_wait(carbide_config.machine_state_controller.uefi_boot_wait)
                .hardware_models(carbide_config.get_firmware_config())
                .firmware_downloader(&downloader)
                .attestation_enabled(carbide_config.attestation_enabled)
                .upload_limiter(upload_limiter.clone())
                .machine_validation_config(carbide_config.machine_validation_config.clone())
                .common_pools(common_pools.clone())
                .bom_validation(carbide_config.bom_validation)
                .no_firmware_update_reset_retries(carbide_config.firmware_global.no_reset_retries)
                .instance_autoreboot_period(
                    carbide_config
                        .machine_updater
                        .instance_autoreboot_period
                        .clone(),
                )
                .credential_reader(api_service.credential_manager.clone())
                .power_options_config(carbide_config.power_manager_options.clone().into())
                .dpf_sdk(dpf_sdk.clone())
                .build(),
        ))
        .io(Arc::new(MachineStateControllerIO {
            host_health: HostHealthConfig {
                hardware_health_reports: carbide_config.host_health.hardware_health_reports,
                dpu_agent_version_staleness_threshold: carbide_config
                    .host_health
                    .dpu_agent_version_staleness_threshold,
                prevent_allocations_on_stale_dpu_agent_version: carbide_config
                    .host_health
                    .prevent_allocations_on_stale_dpu_agent_version,
                prevent_allocations_on_scout_heartbeat_timeout: carbide_config
                    .host_health
                    .prevent_allocations_on_scout_heartbeat_timeout,
                suppress_external_alerting_on_scout_heartbeat_timeout: carbide_config
                    .host_health
                    .suppress_external_alerting_on_scout_heartbeat_timeout,
            },
            sla_config: model::machine::slas::MachineSlaConfig::new(
                carbide_config.machine_state_controller.failure_retry_time,
            ),
        }))
        .state_change_emitter(state_change_emitter)
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build MachineStateController");

    let sc_pool_vlan_id = common_pools.ethernet.pool_vlan_id.clone();
    let sc_pool_vni = common_pools.ethernet.pool_vni.clone();

    let ns_builder = StateController::<NetworkSegmentStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_network_segments", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            NetworkSegmentStateHandlerServices {
                db_pool: db_pool.clone(),
            }
            .into(),
        );
    ns_builder
        .per_object_state_metrics(per_object_state_recorder("network_segment"))
        .iteration_config((&carbide_config.network_segment_state_controller.controller).into())
        .state_handler(Arc::new(NetworkSegmentStateHandler::new(
            carbide_config
                .network_segment_state_controller
                .network_segment_drain_time,
            sc_pool_vlan_id,
            sc_pool_vni,
        )))
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build NetworkSegmentController");

    StateController::<VpcPrefixStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_vpc_prefixes", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            VpcPrefixStateHandlerServices {
                db_pool: db_pool.clone(),
            }
            .into(),
        )
        .per_object_state_metrics(per_object_state_recorder("vpc_prefix"))
        .iteration_config((&carbide_config.vpc_prefix_state_controller.controller).into())
        .state_handler(Arc::new(VpcPrefixStateHandler::new(
            carbide_config
                .vpc_prefix_state_controller
                .vpc_prefix_drain_time,
        )))
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build VpcPrefixStateController");

    StateController::<ExtensionServiceStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_extension_services", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            ExtensionServiceStateHandlerServices {
                db_pool: db_pool.clone(),
                dpf_sdk: dpf_sdk.clone(),
            }
            .into(),
        )
        .per_object_state_metrics(per_object_state_recorder("extension_service"))
        .iteration_config((&carbide_config.extension_service_state_controller.controller).into())
        .state_handler(Arc::new(ExtensionServiceStateHandler))
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build ExtensionServiceStateController");

    if carbide_config.spdm.enabled {
        let Some(nras_config) = carbide_config.spdm.nras_config.clone() else {
            return Err(eyre::eyre!(
                "SPDM attestation is enabled but NRAS config is missing!!"
            ));
        };

        let verifier = Arc::new(VerifierImpl::default());

        StateController::<SpdmStateControllerIO>::builder()
            .database(db_pool.clone(), work_lock_manager_handle.clone())
            .meter("carbide_spdm_attestation", meter.clone())
            .processor_id(state_controller_id.clone())
            .services(
                SpdmStateHandlerServices {
                    db_pool: db_pool.clone(),
                    redfish_client_pool: shared_redfish_pool.clone(),
                }
                .into(),
            )
            .per_object_state_metrics(per_object_state_recorder("spdm_attestation"))
            .iteration_config((&carbide_config.spdm_state_controller.controller).into())
            .state_handler(Arc::new(SpdmAttestationStateHandler::new(
                verifier,
                nras_config,
            )))
            .build_and_spawn(join_set, cancel_token.clone())
            .expect("Unable to build SpdmStateController");
    }

    StateController::<IBPartitionStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_ib_partitions", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            IBPartitionStateHandlerServices {
                db_pool: db_pool.clone(),
                ib_fabric_manager: ib_fabric_manager.clone(),
                ib_pools: common_pools.infiniband.clone(),
            }
            .into(),
        )
        .per_object_state_metrics(per_object_state_recorder("ib_partition"))
        .iteration_config((&carbide_config.ib_partition_state_controller.controller).into())
        .state_handler(Arc::new(IBPartitionStateHandler::default()))
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build IBPartitionStateController");

    StateController::<PowerShelfStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_power_shelves", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            PowerShelfStateHandlerServices {
                db_pool: db_pool.clone(),
                component_manager: component_manager.clone().map(Arc::new),
                credential_manager: credential_manager.clone(),
                per_object_metrics_registry: per_object_metrics_registry.clone(),
                rack_firmware_reprovisioning_enabled: carbide_config
                    .power_shelf_state_controller
                    .rack_firmware_reprovisioning_enabled,
                redfish_client_pool: shared_redfish_pool.clone(),
                bmc_credential_ops: bmc_credential_ops.clone(),
                bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::Bmc,
                ),
                bmc_rotation_enabled: carbide_config.bmc_rotation_enabled,
            }
            .into(),
        )
        .per_object_state_metrics(per_object_state_recorder("power_shelf"))
        .iteration_config((&carbide_config.power_shelf_state_controller.controller).into())
        .state_handler(Arc::new(PowerShelfStateHandler::default()))
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build PowerShelfStateController");

    let default_redirect_policy = reqwest::redirect::Policy::default();

    let firmware_object_fetcher = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let initial_origin = attempt.previous().first().map(reqwest::Url::origin);

            if initial_origin == Some(attempt.url().origin()) {
                default_redirect_policy.redirect(attempt)
            } else {
                attempt.error("firmware-object redirect changed the configured origin")
            }
        }))
        .build()
        .wrap_err("failed to build the firmware-object HTTP client")?;

    StateController::<RackStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_racks", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            RackStateHandlerServices {
                db_pool: db_pool.clone(),
                rms_client: rms_client.clone(),
                site_config: RackConfig {
                    rms: carbide_config.rms.clone(),
                    rack_validation_config: carbide_config.rack_validation_config.clone(),
                    rack_profiles: carbide_config.rack_profiles.clone(),
                }
                .into(),
                nvos_update_manager: nvos_update_manager.clone(),
                credential_manager: credential_manager.clone(),
                component_manager: component_manager.clone().map(Arc::new),
                nmx_cluster_switch_mtls_services: carbide_config
                    .rack_state_controller
                    .effective_nmx_cluster_switch_mtls_services_as_i32(),
                firmware_object_fetcher: Arc::new(firmware_object_fetcher),
                per_object_metrics_registry: per_object_metrics_registry.clone(),
            }
            .into(),
        )
        .per_object_state_metrics(per_object_state_recorder("rack"))
        .iteration_config((&carbide_config.rack_state_controller.controller).into())
        .state_handler(Arc::new(RackStateHandler::default()))
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build RackStateController");

    StateController::<SwitchStateControllerIO>::builder()
        .database(db_pool.clone(), work_lock_manager_handle.clone())
        .meter("carbide_switches", meter.clone())
        .processor_id(state_controller_id.clone())
        .services(
            SwitchStateHandlerServices {
                db_pool: db_pool.clone(),
                component_manager: component_manager.clone().map(Arc::new),
                credential_manager: credential_manager.clone(),
                switch_mtls_services: carbide_config
                    .switch_state_controller
                    .effective_switch_mtls_services_as_i32(),
                per_object_metrics_registry: per_object_metrics_registry.clone(),
                redfish_client_pool: shared_redfish_pool.clone(),
                bmc_credential_ops: bmc_credential_ops.clone(),
                bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                    db::credential_rotation::CredentialRotationType::Bmc,
                ),
                bmc_rotation_enabled: carbide_config.bmc_rotation_enabled,
            }
            .into(),
        )
        .per_object_state_metrics(per_object_state_recorder("switch"))
        .iteration_config((&carbide_config.switch_state_controller.controller).into())
        .state_handler(Arc::new(SwitchStateHandler::default()))
        .build_and_spawn(join_set, cancel_token.clone())
        .expect("Unable to build SwitchStateController");

    IbFabricMonitor::new(
        db_pool.clone(),
        if ib_config.enabled {
            carbide_config.ib_fabrics.clone()
        } else {
            Default::default()
        },
        meter.clone(),
        ib_fabric_manager.clone(),
        carbide_config.host_health,
        work_lock_manager_handle.clone(),
    )
    .start(join_set, cancel_token.clone())?;

    NvLinkManager::new(NvLinkManagerArgs {
        db_pool: db_pool.clone(),
        nmxc_client_pool: api_service.nmxc_client_pool.clone(),
        meter: meter.clone(),
        config: carbide_config.nvlink_config.clone().unwrap_or_default(),
        host_health: carbide_config.host_health,
        component_manager: api_service.component_manager.clone().map(Arc::new),
        work_lock_manager_handle: work_lock_manager_handle.clone(),
    })
    .start(join_set, cancel_token.clone())?;

    if carbide_config.is_ewethers_enabled() {
        let subnet_ip = carbide_config.get_dpa_subnet_ip()?;

        let subnet_mask = carbide_config.get_dpa_subnet_mask()?;

        if !carbide_config.is_svpc_enabled() && !carbide_config.is_astra_enabled() {
            tracing::info!(
                "No EastWest Ethernets config is enabled but neither SVPC nor Astra is enabled. Skipping DPA setup."
            );
        }

        let mut info: DpaInfo = DpaInfo {
            subnet_ip,
            subnet_mask,
            mqtt_client: None,
        };

        if carbide_config.is_svpc_enabled() {
            let svpc_mqtt_client =
                start_svpc_handler(join_set, api_service.clone(), cancel_token.clone()).await?;
            svpc_mqtt_client.register_metrics(&meter, "dpa");

            info.mqtt_client = Some(svpc_mqtt_client);

            tracing::info!("SVPC MQTT client started for SVPC");
        }

        if carbide_config.is_svpc_enabled() || carbide_config.is_astra_enabled() {
            let dpa_info = Arc::new(info);

            DpaMonitor::new(
                db_pool.clone(),
                db_pool.clone().into(),
                dpa_info,
                meter.clone(),
                carbide_config.ewethers_config.clone().unwrap_or_default(),
                carbide_config.host_health,
                work_lock_manager_handle.clone(),
            )
            .start(join_set, cancel_token.clone())?;

            tracing::info!(
                "EastWest Ethernets monitor started for fabrics: subnet_ip: {:#?} subnet_mask: {:#?}",
                subnet_ip,
                subnet_mask
            );
        }
    }

    let site_explorer_config = {
        let mut config = carbide_config.site_explorer.clone();
        // `retained_boot_interface_window` is a single top-level knob
        // (retention spans DHCP, deletion, and ingestion -- it isn't a
        // site-explorer feature). Site-explorer's copy is `#[serde(skip)]`,
        // so it can't be set under `[site_explorer]`; this hand-off is the
        // only way the value gets in, sparing a constructor parameter
        // through `SiteExplorer::new` and every test fixture.
        config.retained_boot_interface_window = carbide_config.retained_boot_interface_window;
        if let Some(window) = config.retained_boot_interface_window {
            tracing::info!(
                window_seconds = window.num_seconds(),
                "retained_boot_interface_window configured; retained boot interface \
                 records expire instead of waiting forever"
            );
        }
        config
    };
    SiteExplorer::new(
        db_pool.clone(),
        site_explorer_config,
        meter.clone(),
        endpoint_exploration_service.clone(),
        api_service.bmc_client.clone(),
        common_pools.clone(),
        work_lock_manager_handle.clone(),
        carbide_config.rack_profiles.clone(),
        rms_client.clone(),
        credential_manager.clone(),
    )
    .start(join_set, cancel_token.clone())?;

    MachineUpdateManager::new(
        db_pool.clone(),
        carbide_config.clone(),
        meter.clone(),
        work_lock_manager_handle.clone(),
        dpf_sdk.clone(),
    )
    .start(join_set, cancel_token.clone())?;

    PreingestionManager::new(
        db_pool.clone(),
        carbide_config.preingestion_manager(),
        shared_redfish_pool.clone(),
        meter.clone(),
        Some(downloader.clone()),
        Some(upload_limiter),
        Some(api_service.credential_manager.clone()),
        work_lock_manager_handle.clone(),
        carbide_config.ntp_servers.clone(),
    )
    .start(join_set, cancel_token.clone())?;

    MeasuredBootMetricsCollector::new(
        db_pool.clone(),
        carbide_config.measured_boot_collector.clone(),
        meter.clone(),
    )
    .start(join_set, cancel_token.clone())?;

    // we need to create ek_cert_status entries for all existing machines
    attestation::backfill_ek_cert_status_for_existing_machines(db_pool).await?;

    crate::machine_validation::MachineValidationManager::new(
        db_pool.clone(),
        carbide_config.machine_validation_config.clone(),
        meter.clone(),
    )
    .start(join_set, cancel_token.clone())?;

    apply_config_on_startup(
        &api_service,
        &carbide_config.machine_validation_config.clone(),
    )
    .await?;

    tracing::info!("initialize_and_start_controllers: all controllers initialized and started");

    Ok(())
}

fn nmxc_tls_config_from_nvlink(
    cfg: &carbide_nvlink_manager::config::NvLinkConfig,
) -> Option<libnmxc::NmxcTlsConfig> {
    let ca = cfg.nmx_c_tls_ca_cert_path.as_ref().map(PathBuf::from);
    let client_cert = cfg.nmx_c_tls_client_cert_path.as_ref().map(PathBuf::from);
    let client_key = cfg.nmx_c_tls_client_key_path.as_ref().map(PathBuf::from);
    if ca.is_none()
        && client_cert.is_none()
        && client_key.is_none()
        && cfg.nmx_c_tls_authority.is_none()
    {
        return None;
    }
    Some(libnmxc::NmxcTlsConfig {
        ca_cert_path: ca,
        client_cert_path: client_cert,
        client_key_path: client_key,
        authority: cfg.nmx_c_tls_authority.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::path::Path;

    use carbide_network::virtualization::VpcVirtualizationType;
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};
    use figment::Figment;
    use figment::providers::{Format, Toml};
    use model::expected_machine::HostDpuPolicy;
    use model::network_segment::{NetworkDefinition, NetworkDefinitionSegmentType};
    use model::resource_pool::ResourcePoolType;
    use model::resource_pool::define::ResourcePoolDef;

    use super::*;
    use crate::cfg::file::{
        CarbideConfig, DpfInterfaceIdentity, HostInterceptBridging, HostRepresentorBridgingConfig,
        InitialObjectsConfig, VmaasConfig, default_hbn_bridge,
    };
    use crate::cfg::load::{merged_carbide_config_figment, parse_carbide_config};

    /// Provides one intercept-bridging config entry for DPF normalization tests.
    fn test_intercept_config(interface: HostInterceptBridging) -> VmaasConfig {
        VmaasConfig {
            allow_instance_vf: true,
            hbn_reps: Some("legacy-only-value".to_string()),
            bridging: Some(HostRepresentorBridgingConfig {
                hbn_bridge: default_hbn_bridge(),
                host_representor_intercept_bridging: HashMap::from([(
                    "legacy-map-key".to_string(),
                    interface,
                )]),
            }),
        }
    }

    /// Provides one typed PF or VF identity for DPF normalization tests.
    fn test_dpf_identity(vf_id: Option<u8>) -> DpfInterfaceIdentity {
        DpfInterfaceIdentity {
            controller_id: 2,
            pf_id: 3,
            vf_id,
        }
    }

    /// Verifies static inventory remains available without VMaaS configuration while every
    /// configured DPF replacement topology contains the PF required by single-interface FMDS.
    #[test]
    fn dpf_intercept_bridging_normalization_requires_configured_pf() {
        // Absence retains the SDK's static PF/VF inventory mode.
        assert!(
            normalize_dpf_intercept_bridging(None, 16)
                .unwrap()
                .is_none()
        );

        value_scenarios!(
            run = |config| normalize_dpf_intercept_bridging(Some(&config), 16).is_err();
            "missing bridging block" {
                // A configured DPF replacement inventory must expose the PF used by FMDS.
                VmaasConfig {
                    allow_instance_vf: false,
                    hbn_reps: None,
                    bridging: None,
                } => true,
            }

            "empty bridging map" {
                // Fixed physical uplinks cannot replace the required host PF FMDS endpoint.
                VmaasConfig {
                    allow_instance_vf: false,
                    hbn_reps: None,
                    bridging: Some(HostRepresentorBridgingConfig {
                        hbn_bridge: default_hbn_bridge(),
                        host_representor_intercept_bridging: HashMap::new(),
                    }),
                } => true,
            }

            "VF-only bridging map" {
                // VFs share the selected parent but cannot supply FMDS's PF interface.
                test_intercept_config(HostInterceptBridging {
                    bridge: "br-vf3".to_string(),
                    patch_port: "p-vf3".to_string(),
                    skip_create: false,
                    dpf_interface: Some(test_dpf_identity(Some(3))),
                }) => true,
            }
        );
    }

    /// Verifies DPF normalization uses typed identity and ignores legacy-only values.
    #[test]
    fn dpf_intercept_bridging_normalization_ignores_legacy_identity_and_hbn_reps() {
        // Build two configs differing only in legacy map key and HBN selection.
        let interface = HostInterceptBridging {
            bridge: "br-pf3".to_string(),
            patch_port: "p-pf3".to_string(),
            skip_create: false,
            dpf_interface: Some(test_dpf_identity(None)),
        };
        let first = test_intercept_config(interface.clone());
        let mut second = test_intercept_config(interface);
        second.hbn_reps = Some("different-legacy-value".to_string());
        let bridging = second.bridging.as_mut().unwrap();
        let entry = bridging
            .host_representor_intercept_bridging
            .remove("legacy-map-key")
            .unwrap();
        bridging
            .host_representor_intercept_bridging
            .insert("unrelated-key".to_string(), entry);

        // Only typed identity and normalized topology values may affect DPF output.
        assert_eq!(
            normalize_dpf_intercept_bridging(Some(&first), 16).unwrap(),
            normalize_dpf_intercept_bridging(Some(&second), 16).unwrap()
        );
    }

    /// Verifies incomplete or skipped entries are rejected only at the DPF boundary.
    #[test]
    fn dpf_intercept_bridging_normalization_rejects_missing_identity_and_skip_create() {
        value_scenarios!(
            // The table evaluates `is_err`, so `true` means normalization rejected the entry.
            run = |interface| normalize_dpf_intercept_bridging(
                Some(&test_intercept_config(interface)),
                16,
            ).is_err();
            "missing typed identity" {
                // Legacy-only entries cannot select a DPF PF or VF safely.
                HostInterceptBridging {
                    bridge: "br-host".to_string(),
                    patch_port: "p-host".to_string(),
                    skip_create: false,
                    dpf_interface: None,
                } => true,
            }

            "skipped entry" {
                // Startup topology is declarative under DPF, so skipped entries are unsupported.
                HostInterceptBridging {
                    bridge: "br-host".to_string(),
                    patch_port: "p-host".to_string(),
                    skip_create: true,
                    dpf_interface: Some(test_dpf_identity(None)),
                } => true,
            }
        );
    }

    #[test]
    fn firmware_object_redirects_require_same_origin() {
        value_scenarios!(run = |(initial, redirect)| {
            let initial = reqwest::Url::parse(initial).expect("initial test URL must parse");
            let redirect = reqwest::Url::parse(redirect).expect("redirect test URL must parse");

            initial.origin() == redirect.origin()
        };
            "same-origin redirects are allowed" {
                (
                    "https://firmware.example.test/object.json",
                    "https://firmware.example.test/releases/object.json",
                ) => true,
                (
                    "https://firmware.example.test/object.json",
                    "https://firmware.example.test:443/object.json",
                ) => true,
            }

            "origin changes are rejected" {
                (
                    "https://firmware.example.test/object.json",
                    "http://firmware.example.test/object.json",
                ) => false,
                (
                    "https://firmware.example.test/object.json",
                    "https://mirror.example.test/object.json",
                ) => false,
                (
                    "https://firmware.example.test/object.json",
                    "https://firmware.example.test:8443/object.json",
                ) => false,
            }
        );
    }

    #[derive(Clone, Copy)]
    struct PolicyLayers {
        global_setting: &'static str,
        site_setting: &'static str,
        environment_value: Option<&'static str>,
    }

    #[allow(clippy::result_large_err)] // Figment controls the error representation.
    fn load_layered_dpu_policy(
        layers: PolicyLayers,
    ) -> Result<Option<HostDpuPolicy>, figment::Error> {
        let mut policy = None;
        figment::Jail::try_with(|jail| {
            jail.clear_env();
            if let Some(environment_value) = layers.environment_value {
                jail.set_env("CARBIDE_API_SITE_EXPLORER", environment_value);
            }

            let global_config = format!(
                "{}\n[site_explorer]\n{}\n",
                include_str!("cfg/test_data/min_config.toml"),
                layers.global_setting,
            );
            let site_config = format!("[site_explorer]\n{}\n", layers.site_setting);
            jail.create_file("global.toml", &global_config)?;
            jail.create_file("site.toml", &site_config)?;

            policy = Some(
                merged_carbide_config_figment(
                    Path::new("global.toml"),
                    Some(Path::new("site.toml")),
                )
                .extract::<CarbideConfig>()?
                .site_explorer
                .dpu_policy,
            );
            Ok(())
        })?;

        Ok(policy.expect("Jail must run the configuration extraction"))
    }

    #[test]
    fn site_explorer_dpu_policy_respects_provider_precedence_across_legacy_key() {
        scenarios!(
            run = load_layered_dpu_policy;
            "site config overrides global config regardless of key spelling" {
                PolicyLayers {
                    global_setting: "dpu_policy = \"nic\"",
                    site_setting: "dpu_mode = \"no_dpu\"",
                    environment_value: None,
                } => Yields(Some(HostDpuPolicy::Ignore)),
                PolicyLayers {
                    global_setting: "dpu_mode = \"no_dpu\"",
                    site_setting: "dpu_policy = \"manage\"",
                    environment_value: None,
                } => Yields(Some(HostDpuPolicy::Manage)),
            }

            "environment overrides site config regardless of key spelling" {
                PolicyLayers {
                    global_setting: "",
                    site_setting: "dpu_policy = \"nic\"",
                    environment_value: Some("{dpu_mode=no_dpu}"),
                } => Yields(Some(HostDpuPolicy::Ignore)),
                PolicyLayers {
                    global_setting: "",
                    site_setting: "dpu_mode = \"no_dpu\"",
                    environment_value: Some("{dpu_policy=manage}"),
                } => Yields(Some(HostDpuPolicy::Manage)),
            }

            "canonical key wins within one provider" {
                PolicyLayers {
                    global_setting: concat!(
                        "dpu_policy = \"nic\"\n",
                        "dpu_mode = \"no_dpu\"",
                    ),
                    site_setting: "",
                    environment_value: None,
                } => Yields(Some(HostDpuPolicy::Nic)),
            }
        );
    }

    fn minimal_carbide_config() -> CarbideConfig {
        Figment::new()
            .merge(Toml::string(
                r#"
                    database_url = "postgres://test"
                    listen = "[::]:1081"
                    asn = 1
                "#,
            ))
            .extract()
            .expect("minimal CarbideConfig parses")
    }

    #[test]
    fn dpf_labels_include_the_derived_gb200_deployment() {
        let config = minimal_carbide_config();
        let labels = build_deployment_type_labels(&config);
        let gb200_label = format!("{}-gb200", config.dpf.deployments.bf3.node_label_key);

        assert_eq!(
            labels.get(&DpuDeploymentType::Bf3Gb200),
            Some(&BTreeMap::from([
                (
                    carbide_dpf::DPU_ENABLED_NODE_LABEL.to_string(),
                    "true".to_string()
                ),
                (gb200_label, "true".to_string()),
            ]))
        );
    }

    fn network_definition(mtu: i32) -> NetworkDefinition {
        let prefix = "10.0.0.0/24"
            .parse::<ipnetwork::IpNetwork>()
            .expect("test network prefix parses");
        NetworkDefinition {
            segment_type: NetworkDefinitionSegmentType::Admin,
            prefix,
            prefix_v6: None,
            gateway: prefix.network(),
            dhcpv6_link_address: None,
            mtu,
            reserve_first: 0,
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
            vpc_name: None,
        }
    }

    fn ipv4_pool(prefix: &str) -> ResourcePoolDef {
        ResourcePoolDef {
            ranges: Vec::new(),
            prefix: Some(prefix.to_string()),
            pool_type: ResourcePoolType::Ipv4,
            delegate_prefix_len: None,
        }
    }

    fn vpc_definition(network_virtualization_type: VpcVirtualizationType) -> VpcDefinition {
        VpcDefinition {
            organization_id: None,
            network_virtualization_type,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: None,
        }
    }

    fn seed_map<T: Clone>(entries: &[(&str, T)]) -> HashMap<String, T> {
        entries
            .iter()
            .map(|(name, definition)| (name.to_string(), definition.clone()))
            .collect()
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestSeed(u8);

    impl SeedKind for TestSeed {
        fn name() -> &'static str {
            "Test object"
        }

        fn source_description(_: &CarbideConfig, _: &str) -> String {
            "legacy test config".to_string()
        }
    }

    fn test_seed_map(entries: &[(&str, u8)]) -> HashMap<String, TestSeed> {
        entries
            .iter()
            .map(|(name, value)| (name.to_string(), TestSeed(*value)))
            .collect()
    }

    struct MergeSources {
        initial_objects: Option<HashMap<String, TestSeed>>,
        carbide_config: Option<HashMap<String, TestSeed>>,
        required: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum MergeFailure {
        MissingRequired,
        Conflicts(BTreeSet<String>),
        Unexpected(String),
    }

    fn merge_test_seeds(
        input: MergeSources,
        config: &CarbideConfig,
    ) -> Result<BTreeMap<String, u8>, MergeFailure> {
        let candidate_names: BTreeSet<String> = input
            .initial_objects
            .iter()
            .chain(input.carbide_config.iter())
            .flat_map(|objects| objects.keys().cloned())
            .collect();

        match SeedData::merge_objects(
            input.initial_objects.as_ref(),
            input.carbide_config.as_ref(),
            config,
            input.required,
        ) {
            Ok(objects) => Ok(objects
                .iter()
                .map(|(name, TestSeed(value))| (name.clone(), *value))
                .collect()),
            Err(error)
                if matches!(
                    error.downcast_ref::<DefineResourcePoolError>(),
                    Some(DefineResourcePoolError::InvalidArgument(_))
                ) =>
            {
                Err(MergeFailure::MissingRequired)
            }
            Err(error) => {
                let message = error.to_string();
                let conflicts = candidate_names
                    .into_iter()
                    .filter(|name| message.contains(&format!("`{name}`")))
                    .collect::<BTreeSet<_>>();

                if conflicts.is_empty() {
                    Err(MergeFailure::Unexpected(message))
                } else {
                    Err(MergeFailure::Conflicts(conflicts))
                }
            }
        }
    }

    #[derive(Clone, Copy)]
    enum SeedObjectKind {
        Network,
        Vpc,
        Pool,
    }

    #[derive(Clone, Copy)]
    enum SeedSource {
        InitialObjects,
        LegacyConfig,
    }

    #[derive(Clone, Copy)]
    enum ResolveInput {
        Object {
            kind: SeedObjectKind,
            source: SeedSource,
        },
        ConflictingNetwork,
        ConflictingVpc,
        InvalidNetwork,
        InvalidVpcOverrides {
            source: SeedSource,
        },
        NoOptionalObjects,
        MissingPools,
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct SeedSummary {
        networks: BTreeSet<String>,
        vpcs: BTreeSet<String>,
        pools: BTreeSet<String>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ResolveFailure {
        Conflict(String),
        InvalidNetwork,
        InvalidVpcOverrides,
        MissingPools,
        Unexpected(String),
    }

    fn classify_config_validation_error(error: &model::ConfigValidationError) -> ResolveFailure {
        match error {
            model::ConfigValidationError::InitialVpcRoutingProfileOverridesUnsupported {
                ..
            } => ResolveFailure::InvalidVpcOverrides,
            // The only other configuration validation performed by
            // `SeedData::resolve` is `NetworkDefinition::validate`.
            model::ConfigValidationError::InvalidValue(_) => ResolveFailure::InvalidNetwork,
            _ => ResolveFailure::Unexpected(error.to_string()),
        }
    }

    fn names(entries: &[&str]) -> BTreeSet<String> {
        entries.iter().map(|name| name.to_string()).collect()
    }

    fn seed_summary(networks: &[&str], vpcs: &[&str], pools: &[&str]) -> SeedSummary {
        SeedSummary {
            networks: names(networks),
            vpcs: names(vpcs),
            pools: names(pools),
        }
    }

    fn resolve_seed_data(input: ResolveInput) -> Result<SeedSummary, ResolveFailure> {
        let mut config = minimal_carbide_config();
        config.networks = None;
        config.vpcs = None;
        config.pools = Some(HashMap::new());

        let mut initial_objects = InitialObjectsConfig {
            pools: None,
            networks: None,
            vpcs: None,
        };
        let mut use_initial_objects = false;

        match input {
            ResolveInput::Object {
                kind: SeedObjectKind::Network,
                source: SeedSource::InitialObjects,
            } => {
                initial_objects.networks =
                    Some(seed_map(&[("test-network", network_definition(1500))]));
                use_initial_objects = true;
            }
            ResolveInput::Object {
                kind: SeedObjectKind::Network,
                source: SeedSource::LegacyConfig,
            } => {
                config.networks = Some(seed_map(&[("test-network", network_definition(1500))]));
            }
            ResolveInput::Object {
                kind: SeedObjectKind::Vpc,
                source: SeedSource::InitialObjects,
            } => {
                initial_objects.vpcs = Some(seed_map(&[(
                    "test-vpc",
                    vpc_definition(VpcVirtualizationType::Flat),
                )]));
                use_initial_objects = true;
            }
            ResolveInput::Object {
                kind: SeedObjectKind::Vpc,
                source: SeedSource::LegacyConfig,
            } => {
                config.vpcs = Some(seed_map(&[(
                    "test-vpc",
                    vpc_definition(VpcVirtualizationType::Flat),
                )]));
            }
            ResolveInput::Object {
                kind: SeedObjectKind::Pool,
                source: SeedSource::InitialObjects,
            } => {
                config.pools = None;
                initial_objects.pools = Some(seed_map(&[("test-pool", ipv4_pool("10.0.0.0/24"))]));
                use_initial_objects = true;
            }
            ResolveInput::Object {
                kind: SeedObjectKind::Pool,
                source: SeedSource::LegacyConfig,
            } => {
                config.pools = Some(seed_map(&[("test-pool", ipv4_pool("10.0.0.0/24"))]));
            }
            ResolveInput::ConflictingNetwork => {
                initial_objects.networks =
                    Some(seed_map(&[("test-network", network_definition(1500))]));
                config.networks = Some(seed_map(&[("test-network", network_definition(1600))]));
                use_initial_objects = true;
            }
            ResolveInput::ConflictingVpc => {
                initial_objects.vpcs = Some(seed_map(&[(
                    "test-vpc",
                    vpc_definition(VpcVirtualizationType::Flat),
                )]));
                config.vpcs = Some(seed_map(&[(
                    "test-vpc",
                    vpc_definition(VpcVirtualizationType::EthernetVirtualizer),
                )]));
                use_initial_objects = true;
            }
            ResolveInput::InvalidNetwork => {
                config.networks = Some(seed_map(&[("test-network", network_definition(9214))]));
            }
            ResolveInput::InvalidVpcOverrides {
                source: SeedSource::InitialObjects,
            } => {
                initial_objects.vpcs = Some(seed_map(&[(
                    "test-vpc",
                    VpcDefinition {
                        routing_profile_overrides: Some(Default::default()),
                        ..vpc_definition(VpcVirtualizationType::Fnn)
                    },
                )]));
                use_initial_objects = true;
            }
            ResolveInput::InvalidVpcOverrides {
                source: SeedSource::LegacyConfig,
            } => {
                config.vpcs = Some(seed_map(&[(
                    "test-vpc",
                    VpcDefinition {
                        routing_profile_overrides: Some(Default::default()),
                        ..vpc_definition(VpcVirtualizationType::Fnn)
                    },
                )]));
            }
            ResolveInput::NoOptionalObjects => {}
            ResolveInput::MissingPools => {
                config.pools = None;
            }
        }

        match SeedData::resolve(&config, use_initial_objects.then_some(&initial_objects)) {
            Ok(seed_data) => Ok(SeedSummary {
                networks: seed_data.initial_networks.keys().cloned().collect(),
                vpcs: seed_data.initial_vpcs.keys().cloned().collect(),
                pools: seed_data.initial_pools.keys().cloned().collect(),
            }),
            Err(error) => {
                if let Some(DefineResourcePoolError::InvalidArgument(message)) =
                    error.downcast_ref::<DefineResourcePoolError>()
                    && message.to_ascii_lowercase().contains("no resource pools")
                {
                    return Err(ResolveFailure::MissingPools);
                }
                if let Some(error) = error.downcast_ref::<model::ConfigValidationError>() {
                    return Err(classify_config_validation_error(error));
                }

                let message = error.to_string();
                if let Some(name) = ["test-network", "test-vpc"]
                    .into_iter()
                    .find(|name| message.contains(&format!("`{name}`")))
                {
                    Err(ResolveFailure::Conflict(name.to_string()))
                } else {
                    Err(ResolveFailure::Unexpected(message))
                }
            }
        }
    }

    #[test]
    #[allow(clippy::result_large_err)] // Figment controls the error representation.
    fn rack_profile_attributes_merge_per_key_with_provider_precedence() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            // Environment input replaces only its duplicate key; unrelated
            // keys from lower-priority providers remain in the effective map.
            jail.set_env(
                "CARBIDE_API_RACK_PROFILES",
                "{NVL72={attributes={attribute1=environment,additional_attribute4=environment}}}",
            );

            let global_config = format!(
                r#"{}

[rack_profiles.NVL72]
product_family = "gb200"
attributes = {{ attribute1 = "global", additional_attribute2 = "global" }}

[rack_profiles.NVL72.rack_capabilities.compute]
count = 18

[rack_profiles.NVL72.rack_capabilities.switch]
count = 9

[rack_profiles.NVL72.rack_capabilities.power_shelf]
count = 8
"#,
                include_str!("cfg/test_data/min_config.toml"),
            );

            let site_config = r#"
[rack_profiles.NVL72]
attributes = { attribute1 = "site", additional_attribute3 = "site" }
"#;
            jail.create_file("global.toml", &global_config)?;
            jail.create_file("site.toml", site_config)?;

            let config = merged_carbide_config_figment(
                Path::new("global.toml"),
                Some(Path::new("site.toml")),
            )
            .extract::<CarbideConfig>()?;

            let attributes = &config.rack_profiles.get("NVL72").unwrap().attributes;

            assert_eq!(
                attributes,
                &HashMap::from([
                    ("attribute1".to_string(), "environment".to_string()),
                    ("additional_attribute2".to_string(), "global".to_string()),
                    ("additional_attribute3".to_string(), "site".to_string()),
                    (
                        "additional_attribute4".to_string(),
                        "environment".to_string(),
                    ),
                ])
            );

            Ok(())
        });
    }

    #[test]
    fn parse_rejects_rms_component_manager_missing_vendor() -> eyre::Result<()> {
        let mut config = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(
            &mut config,
            br#"
                database_url = "postgres://test"
                listen = "[::]:1081"
                asn = 1

                [component_manager]
                nv_switch_backend = "rms"
                power_shelf_backend = "mock"
                compute_tray_backend = "mock"

                [rack_profiles.NVL72]
                product_family = "gb200"
                rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"

                [rack_profiles.NVL72.rack_capabilities.compute]
                name = "GB200"
                count = 18
                vendor = "NVIDIA"

                [rack_profiles.NVL72.rack_capabilities.switch]
                count = 9

                [rack_profiles.NVL72.rack_capabilities.power_shelf]
                count = 8
            "#,
        )?;

        let result = parse_carbide_config(config.path(), None);
        let Err(error) = result else {
            panic!("missing RMS vendor should be rejected");
        };

        let error = format!("{error:?}");

        assert!(
            error.contains("rack profile NVL72 cannot build RMS switch node descriptor")
                && error.contains("rack profile does not identify an RMS switch vendor"),
            "error message should identify the rack profile and missing role vendor: {error}"
        );

        Ok(())
    }

    #[test]
    fn seed_data_merge_objects_enumerates_source_combinations() {
        let config = minimal_carbide_config();

        check_cases(
            [
                Case {
                    scenario: "an optional object kind may be absent",
                    input: MergeSources {
                        initial_objects: None,
                        carbide_config: None,
                        required: false,
                    },
                    expect: Yields(BTreeMap::new()),
                },
                Case {
                    scenario: "a required object kind must be declared",
                    input: MergeSources {
                        initial_objects: None,
                        carbide_config: None,
                        required: true,
                    },
                    expect: FailsWith(MergeFailure::MissingRequired),
                },
                Case {
                    scenario: "initial-objects definitions are authoritative on their own",
                    input: MergeSources {
                        initial_objects: Some(test_seed_map(&[("initial", 1)])),
                        carbide_config: None,
                        required: false,
                    },
                    expect: Yields(BTreeMap::from([("initial".to_string(), 1)])),
                },
                Case {
                    scenario: "legacy definitions are retained on their own",
                    input: MergeSources {
                        initial_objects: None,
                        carbide_config: Some(test_seed_map(&[("legacy", 2)])),
                        required: false,
                    },
                    expect: Yields(BTreeMap::from([("legacy".to_string(), 2)])),
                },
                Case {
                    scenario: "disjoint definitions are merged",
                    input: MergeSources {
                        initial_objects: Some(test_seed_map(&[("initial", 1)])),
                        carbide_config: Some(test_seed_map(&[("legacy", 2)])),
                        required: false,
                    },
                    expect: Yields(BTreeMap::from([
                        ("initial".to_string(), 1),
                        ("legacy".to_string(), 2),
                    ])),
                },
                Case {
                    scenario: "identical overlapping definitions are deduplicated",
                    input: MergeSources {
                        initial_objects: Some(test_seed_map(&[("shared", 1)])),
                        carbide_config: Some(test_seed_map(&[("shared", 1)])),
                        required: false,
                    },
                    expect: Yields(BTreeMap::from([("shared".to_string(), 1)])),
                },
                Case {
                    scenario: "every conflicting definition is reported",
                    input: MergeSources {
                        initial_objects: Some(test_seed_map(&[("alpha", 1), ("beta", 2)])),
                        carbide_config: Some(test_seed_map(&[("alpha", 3), ("beta", 4)])),
                        required: false,
                    },
                    expect: FailsWith(MergeFailure::Conflicts(BTreeSet::from([
                        "alpha".to_string(),
                        "beta".to_string(),
                    ]))),
                },
            ],
            |input| merge_test_seeds(input, &config),
        );
    }

    #[test]
    fn seed_data_resolve_wires_each_object_kind() {
        check_cases(
            [
                Case {
                    scenario: "network from initial-objects config",
                    input: ResolveInput::Object {
                        kind: SeedObjectKind::Network,
                        source: SeedSource::InitialObjects,
                    },
                    expect: Yields(seed_summary(&["test-network"], &[], &[])),
                },
                Case {
                    scenario: "network from legacy config",
                    input: ResolveInput::Object {
                        kind: SeedObjectKind::Network,
                        source: SeedSource::LegacyConfig,
                    },
                    expect: Yields(seed_summary(&["test-network"], &[], &[])),
                },
                Case {
                    scenario: "VPC from initial-objects config",
                    input: ResolveInput::Object {
                        kind: SeedObjectKind::Vpc,
                        source: SeedSource::InitialObjects,
                    },
                    expect: Yields(seed_summary(&[], &["test-vpc"], &[])),
                },
                Case {
                    scenario: "VPC from legacy config",
                    input: ResolveInput::Object {
                        kind: SeedObjectKind::Vpc,
                        source: SeedSource::LegacyConfig,
                    },
                    expect: Yields(seed_summary(&[], &["test-vpc"], &[])),
                },
                Case {
                    scenario: "resource pool from initial-objects config",
                    input: ResolveInput::Object {
                        kind: SeedObjectKind::Pool,
                        source: SeedSource::InitialObjects,
                    },
                    expect: Yields(seed_summary(&[], &[], &["test-pool"])),
                },
                Case {
                    scenario: "resource pool from legacy config",
                    input: ResolveInput::Object {
                        kind: SeedObjectKind::Pool,
                        source: SeedSource::LegacyConfig,
                    },
                    expect: Yields(seed_summary(&[], &[], &["test-pool"])),
                },
                Case {
                    scenario: "optional network and VPC definitions may both be absent",
                    input: ResolveInput::NoOptionalObjects,
                    expect: Yields(SeedSummary::default()),
                },
                Case {
                    scenario: "network conflicts propagate through resolution",
                    input: ResolveInput::ConflictingNetwork,
                    expect: FailsWith(ResolveFailure::Conflict("test-network".to_string())),
                },
                Case {
                    scenario: "VPC conflicts propagate through resolution",
                    input: ResolveInput::ConflictingVpc,
                    expect: FailsWith(ResolveFailure::Conflict("test-vpc".to_string())),
                },
                Case {
                    scenario: "invalid merged networks fail during resolution",
                    input: ResolveInput::InvalidNetwork,
                    expect: FailsWith(ResolveFailure::InvalidNetwork),
                },
                Case {
                    scenario: "initial-objects VPC overrides fail during resolution",
                    input: ResolveInput::InvalidVpcOverrides {
                        source: SeedSource::InitialObjects,
                    },
                    expect: FailsWith(ResolveFailure::InvalidVpcOverrides),
                },
                Case {
                    scenario: "legacy VPC overrides fail during resolution",
                    input: ResolveInput::InvalidVpcOverrides {
                        source: SeedSource::LegacyConfig,
                    },
                    expect: FailsWith(ResolveFailure::InvalidVpcOverrides),
                },
                Case {
                    scenario: "resource pool definitions remain required",
                    input: ResolveInput::MissingPools,
                    expect: FailsWith(ResolveFailure::MissingPools),
                },
            ],
            resolve_seed_data,
        );
    }

    #[test]
    fn seed_resolution_preserves_unexpected_config_validation_errors() {
        let error =
            model::ConfigValidationError::DuplicateTenantKeysetId("duplicate-keyset".to_string());
        assert_eq!(
            classify_config_validation_error(&error),
            ResolveFailure::Unexpected(error.to_string())
        );
    }
}
