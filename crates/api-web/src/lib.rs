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
use std::env;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use askama::Template;
use axum::Extension;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{Router, get, post};
use axum_extra::extract::cookie::{Cookie, Key, PrivateCookieJar};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use carbide_api_core::cfg::file::ToolLink;
use carbide_api_core::{AdminAdmissionControl, Api, AuthContext, CarbideError, DefaultCredential};
use carbide_authn::middleware::Principal;
use http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use http::{HeaderMap, Request, StatusCode};
use itertools::Itertools;
use oauth2::basic::{
    BasicClient, BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse,
    BasicTokenResponse,
};
use oauth2::{
    AuthUrl, Client, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, RedirectUrl, Scope, StandardRevocableToken, TokenUrl,
};
use rand::RngExt as _;
use rpc::forge::forge_server::Forge;
use rpc::forge::{self as forgerpc};
use tonic::service::AxumBody;
use tower_http::csrf::CsrfLayer;
use tower_http::normalize_path::NormalizePath;

/// Implemented by every page struct whose template extends `base.html`.
/// Exposes the global tool list to the shared "Tools" sidebar via
/// `Self::tools()`.
///
/// The tool list itself is owned by `carbide-api-core` (it is derived from the
/// parsed `CarbideConfig` during API Core configuration loading).
/// This trait just surfaces it to templates.
trait Base {
    /// Configured external tool links rendered in the admin UI's
    /// "Tools" sidebar. Empty when no tools are configured or
    /// before API Core configuration loading has initialized them (e.g. unit tests).
    fn tools() -> &'static [ToolLink] {
        carbide_api_core::configured_tools()
    }

    /// Site name rendered in the sidebar header as "NICo • <site>". Falls
    /// back to "local" when the config doesn't set `sitename`.
    fn site_name() -> &'static str {
        carbide_api_core::configured_site_name().unwrap_or("local")
    }

    /// URL template for the "Logs" link on machine and endpoint detail pages.
    /// The placeholder `{search}` is replaced with the machine ID or BMC IP
    /// by JavaScript. Empty when not configured.
    fn logs_link_template() -> &'static str {
        carbide_api_core::configured_logs_link_template()
    }
}

/// Reusable template for rendering metadata (name, description, labels, version)
/// in entity detail pages. Render with `{{ metadata_detail|safe }}`.
#[derive(Template)]
#[template(path = "metadata_details.html")]
struct MetadataDetail {
    metadata: rpc::forge::Metadata,
    metadata_version: String,
}

/// Reusable template for rendering aggregate health details in entity detail
/// pages. Render with `{{ health_detail|safe }}`.
#[derive(Template)]
#[template(path = "health_detail.html")]
struct HealthDetail {
    health_reports_url: String,
    health_reports_link_text: &'static str,
    health: health_report::HealthReport,
    health_sources: Vec<String>,
}

impl HealthDetail {
    fn new(
        health_reports_url: String,
        health_reports_link_text: &'static str,
        health: Option<rpc::health::HealthReport>,
        health_sources: Vec<rpc::forge::HealthSourceOrigin>,
    ) -> Self {
        HealthDetail {
            health_reports_url,
            health_reports_link_text,
            health: health
                .map(|health| {
                    health_report::HealthReport::try_from(health)
                        .unwrap_or_else(health_report::HealthReport::malformed_report)
                })
                .unwrap_or_else(health_report::HealthReport::missing_report),
            health_sources: health_sources
                .into_iter()
                .map(|source| source.source)
                .collect(),
        }
    }
}

/// Reusable template for rendering a color-coded state bubble.
/// Render with `{{ state_display|safe }}`.
#[derive(Debug, Clone, PartialEq, Eq, Template)]
#[template(path = "state_display.html")]
struct StateDisplay {
    state: String,
    time_in_state_above_sla: bool,
}

impl StateDisplay {
    fn from_lifecycle(lifecycle: Option<&forgerpc::LifecycleStatus>) -> Self {
        let state = lifecycle
            .map(|lifecycle| lifecycle.state.clone())
            .filter(|state| !state.is_empty())
            .unwrap_or_else(|| r#"{ "state": "unknown" }"#.to_string());
        let time_in_state_above_sla = lifecycle
            .and_then(|lifecycle| lifecycle.sla.as_ref())
            .map(|sla| sla.time_in_state_above_sla)
            .unwrap_or(false);

        Self {
            state,
            time_in_state_above_sla,
        }
    }
}

/// Reusable template for rendering State SLA, time-in-state-above-SLA, and
/// state handler outcome rows inside a `<table>`.
/// Render with `{{ state_sla_detail|safe }}`.
#[derive(Template)]
#[template(path = "state_sla_details.html")]
struct StateSlaDetail {
    state_sla: String,
    time_in_state_above_sla: bool,
    state_reason: Option<rpc::forge::ControllerStateReason>,
}

/// Reusable template for rendering lifecycle fields.
/// Render with `{{ lifecycle_detail|safe }}`.
#[derive(Template)]
#[template(path = "lifecycle_detail.html")]
struct LifecycleDetail {
    state_display: StateDisplay,
    associated_instance_id: Option<String>,
    json_state: Option<String>,
    version: String,
    time_in_state: String,
    state_sla: String,
    time_in_state_above_sla: bool,
    state_reason: Option<rpc::forge::ControllerStateReason>,
}

impl LifecycleDetail {
    fn new(
        state: String,
        version: String,
        state_reason: Option<forgerpc::ControllerStateReason>,
        sla: Option<forgerpc::StateSla>,
    ) -> Self {
        let time_in_state_above_sla = sla
            .as_ref()
            .map(|sla| sla.time_in_state_above_sla)
            .unwrap_or_default();
        let json_state = verify_json(&state);
        Self {
            state_display: StateDisplay {
                state,
                time_in_state_above_sla,
            },
            associated_instance_id: None,
            json_state,
            time_in_state: config_version::since_state_change_humanized(&version),
            version,
            state_sla: format_state_sla(sla.as_ref()),
            time_in_state_above_sla,
            state_reason,
        }
    }
}

impl From<forgerpc::LifecycleStatus> for LifecycleDetail {
    fn from(lifecycle: forgerpc::LifecycleStatus) -> Self {
        LifecycleDetail::new(
            lifecycle.state,
            lifecycle.version,
            lifecycle.state_reason,
            lifecycle.sla,
        )
    }
}

fn verify_json(state: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(state)
        .ok()
        .map(|_| state.to_string())
}

fn format_state_sla(sla: Option<&forgerpc::StateSla>) -> String {
    sla.and_then(|sla| sla.sla)
        .map(|sla| {
            config_version::format_duration(
                chrono::TimeDelta::try_from(sla).unwrap_or(chrono::TimeDelta::MAX),
            )
        })
        .unwrap_or_default()
}

// Allow `carbide_macros::sqlx_test` to be referred to as
// `#[crate::sqlx_test]` in the admin-UI tests. These tests do not support
// `fixtures(...)`; use explicit setup helpers instead.
#[cfg(test)]
use carbide_macros::sqlx_test;
use carbide_utils::none_if_empty::NoneIfEmpty;

#[cfg(test)]
mod tests;

mod action_status;
mod attestation;
mod auth;
mod compute_allocation;
mod configuration;
mod domain;
mod dpa;
mod dpu_versions;
mod expected_machine;
mod expected_power_shelf;
mod expected_rack;
mod expected_switch;
mod explored_endpoint;
mod filters;
mod firmware;
mod health;
mod health_history;
mod ib_fabric;
mod ib_partition;
mod instance;
mod instance_type;
mod interface;
mod ipam;
mod ipxe_template;
mod logs;
mod machine;
mod machine_validation;
mod managed_host;
mod network_device;
mod network_security_group;
mod network_segment;
mod network_status;
mod nmxc_browser;
mod nvlink;
mod operating_system;
mod pagination;
mod power_shelf;
mod rack;
mod redfish_actions;
mod redfish_browser;
mod resource_pool;
mod search;
mod site_explorer_run_status;
mod sku;
mod spx_partition;
mod state_history;
mod switch;
mod tenant;
mod tenant_keyset;
mod ufm_browser;
mod vpc;

const AUTH_TYPE_ENV: &str = "CARBIDE_WEB_AUTH_TYPE";
const BASIC_AUTH_PASSWORD_ENV: &str = "CARBIDE_WEB_BASIC_AUTH_PASSWORD";
const BASIC_AUTH_USERNAME: &str = "admin";
const BASIC_AUTH_REALM: &str = "Basic realm=\"NICo\"";
const TEMPORARY_BASIC_AUTH_REALM: &str =
    "Basic realm=\"NICo - check service logs for the random admin password\"";
const AUTH_CALLBACK_ROOT: &str = "auth-callback";

// Details https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationMenuBlade/~/Overview/appId/5ae5fa35-be8e-44cc-be7b-01ff76af5315/isMSAApp~/false
const OAUTH2_AUTH_ENDPOINT_ENV: &str = "CARBIDE_WEB_OAUTH2_AUTH_ENDPOINT";

const OAUTH2_TOKEN_ENDPOINT_ENV: &str = "CARBIDE_WEB_OAUTH2_TOKEN_ENDPOINT";

const CARBIDE_WEB_PRIVATE_COOKIEJAR_KEY_ENV: &str = "CARBIDE_WEB_PRIVATE_COOKIEJAR_KEY";
const CARBIDE_WEB_HOSTNAME_ENV: &str = "CARBIDE_WEB_HOSTNAME";

const OAUTH2_CLIENT_SECRET_ENV: &str = "CARBIDE_WEB_OAUTH2_CLIENT_SECRET";
const OAUTH2_CLIENT_ID_ENV: &str = "CARBIDE_WEB_OAUTH2_CLIENT_ID";

const ALLOWED_ACCESS_GROUPS_LIST_ENV: &str = "CARBIDE_WEB_ALLOWED_ACCESS_GROUPS";

const ALLOWED_ACCESS_GROUPS_ID_LIST_ENV: &str = "CARBIDE_WEB_ALLOWED_ACCESS_GROUPS_ID_LIST";

const SORTABLE_JS: &str = include_str!("../templates/static/sortable.min.js");
const SORTABLE_CSS: &str = include_str!("../templates/static/sortable.min.css");
const CARBIDE_CSS: &str = include_str!("../templates/static/carbide.css");
const TABS_JS: &str = include_str!("../templates/static/tabs.js");
const TABLE_FILTER_JS: &str = include_str!("../templates/static/table_filter.js");

// It would appear the oauth2 author read about the typestate pattern and decided making
// everyone declare 10 type parameters when storing a Client sounds like a great idea.
// https://github.com/ramosbugs/oauth2-rs/blob/main/UPGRADE.md#add-typestate-generic-types-to-client
type Oauth2ClientWithPropertiesSet = Client<
    BasicErrorResponse,
    BasicTokenResponse,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

struct Oauth2Layer {
    client: Oauth2ClientWithPropertiesSet,
    http_client: reqwest::Client,
    private_cookiejar_key: Key,
    allowed_access_groups_filter: String,
    allowed_access_groups_ids_to_name: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum WebAuthMode {
    #[default]
    Basic,
    OAuth2,
    None,
}

impl FromStr for WebAuthMode {
    type Err = eyre::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "oauth2" => Ok(Self::OAuth2),
            "none" => Ok(Self::None),
            other => Err(eyre::eyre!(
                "unknown {AUTH_TYPE_ENV}={other:?}: expected \"basic\", \"oauth2\", or \"none\""
            )),
        }
    }
}

enum WebAuth {
    Basic {
        password: String,
        challenge: &'static str,
    },
    OAuth2(Arc<Oauth2Layer>),
    None,
}

impl WebAuth {
    fn basic(configured_password: Option<String>) -> Self {
        match configured_password {
            Some(password) => WebAuth::Basic {
                password,
                challenge: BASIC_AUTH_REALM,
            },
            None => {
                let password: String = rand::rng()
                    .sample_iter(&rand::distr::Alphanumeric)
                    .take(32)
                    .map(char::from)
                    .collect();
                tracing::warn!(
                    username = BASIC_AUTH_USERNAME,
                    password = %password,
                    "{BASIC_AUTH_PASSWORD_ENV} is not configured; generated a temporary WebUI Basic Auth password for this process. This password is not persisted and changes on every launch. Configure a permanent password by setting {BASIC_AUTH_PASSWORD_ENV} to a non-empty value."
                );
                WebAuth::Basic {
                    password,
                    challenge: TEMPORARY_BASIC_AUTH_REALM,
                }
            }
        }
    }
}

/// All the URLs in the admin interface. Nested under /admin in api.rs.
pub fn routes(
    api: Arc<Api>,
    admission: Option<AdminAdmissionControl>,
) -> eyre::Result<NormalizePath<Router>> {
    let auth_mode =
        env::var(AUTH_TYPE_ENV).map_or(Ok(WebAuthMode::default()), |value| value.parse())?;
    routes_with_auth_mode(api, auth_mode, admission)
}

fn routes_with_auth_mode(
    api: Arc<Api>,
    auth_mode: WebAuthMode,
    admission: Option<AdminAdmissionControl>,
) -> eyre::Result<NormalizePath<Router>> {
    let web_auth = match auth_mode {
        WebAuthMode::OAuth2 => {
            // Get our cookiejar key so we can add it as an extension.
            let private_cookiejar_key = Key::try_from(
                env::var(CARBIDE_WEB_PRIVATE_COOKIEJAR_KEY_ENV)
                    .map_err(|e| {
                        CarbideError::internal(format!(
                            "{CARBIDE_WEB_PRIVATE_COOKIEJAR_KEY_ENV}: {e}"
                        ))
                    })?
                    .as_bytes(),
            )?;

            // Grab the details for which groups are allowed to access the web UI.
            let allowed_groups = env::var(ALLOWED_ACCESS_GROUPS_LIST_ENV).map_err(|e| {
                CarbideError::internal(format!("{ALLOWED_ACCESS_GROUPS_LIST_ENV}: {e}"))
            })?;
            let allowed_access_groups_names = allowed_groups.split(",");
            let allowed_access_groups_filter = allowed_access_groups_names
                .clone()
                .map(|s| format!("\"displayName:{}\"", s.to_lowercase()))
                .join(" OR ");
            let allowed_access_groups_ids_to_name = env::var(ALLOWED_ACCESS_GROUPS_ID_LIST_ENV)
                .map_err(|e| {
                    CarbideError::internal(format!("{ALLOWED_ACCESS_GROUPS_ID_LIST_ENV}: {e}"))
                })?
                .split(",")
                .map(|s| s.to_lowercase())
                .zip(allowed_access_groups_names)
                .map(|(id, name)| (id, name.to_string()))
                .collect::<HashMap<String, String>>();

            let client_id = env::var(OAUTH2_CLIENT_ID_ENV)
                .map_err(|e| CarbideError::internal(format!("{OAUTH2_CLIENT_ID_ENV}: {e}")))?;
            let client_secret = env::var(OAUTH2_CLIENT_SECRET_ENV)
                .map_err(|e| CarbideError::internal(format!("{OAUTH2_CLIENT_SECRET_ENV}: {e}")))?;
            let auth_endpoint = env::var(OAUTH2_AUTH_ENDPOINT_ENV)
                .map_err(|e| CarbideError::internal(format!("{OAUTH2_AUTH_ENDPOINT_ENV}: {e}")))?;
            let token_endpoint = env::var(OAUTH2_TOKEN_ENDPOINT_ENV)
                .map_err(|e| CarbideError::internal(format!("{OAUTH2_TOKEN_ENDPOINT_ENV}: {e}")))?;

            // Build the  OAuth2 client.
            let client = BasicClient::new(ClientId::new(client_id))
                .set_client_secret(ClientSecret::new(client_secret))
                .set_auth_uri(AuthUrl::new(auth_endpoint)?)
                .set_token_uri(TokenUrl::new(token_endpoint)?)
                .set_redirect_uri(RedirectUrl::new(format!(
                    "https://{}/admin/{}",
                    env::var(CARBIDE_WEB_HOSTNAME_ENV).unwrap_or("localhost:1079".to_string()),
                    AUTH_CALLBACK_ROOT,
                ))?);

            let http_client = {
                let builder = reqwest::Client::builder();
                let builder = builder
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::new(5, 0)) // Limit connections to 5 seconds
                    .timeout(Duration::new(15, 0)); // Limit the overall request to 15 seconds

                builder.build()?
            };

            WebAuth::OAuth2(Arc::new(Oauth2Layer {
                client,
                private_cookiejar_key,
                allowed_access_groups_filter,
                allowed_access_groups_ids_to_name,
                http_client,
            }))
        }
        WebAuthMode::None => {
            tracing::warn!(
                auth_type_env_var = AUTH_TYPE_ENV,
                "admin web UI has no in-process authentication; restrict access with network policy, a private network, or an authenticating reverse proxy (for example OAuth2 Proxy)",
            );
            WebAuth::None
        }
        WebAuthMode::Basic => {
            WebAuth::basic(env::var(BASIC_AUTH_PASSWORD_ENV).ok().none_if_empty())
        }
    };

    let oauth_extension_layer = match &web_auth {
        WebAuth::OAuth2(layer) => Some(Arc::clone(layer)),
        WebAuth::Basic { .. } | WebAuth::None => None,
    };

    Ok(NormalizePath::trim_trailing_slash(
        Router::new()
            .route("/", get(root))
            .route("/static/{filename}", get(static_data))
            .route("/domain", get(domain::show_html))
            .route("/domain.json", get(domain::show_all_json))
            .route("/dpa", get(dpa::show_dpas_html))
            .route("/dpa.json", get(dpa::show_dpas_json))
            .route("/dpa/{dpa_id}", get(dpa::detail))
            .route("/dpu", get(machine::show_dpus_html))
            .route("/dpu.json", get(machine::show_dpus_json))
            .route("/dpu/versions", get(dpu_versions::list_html))
            .route("/dpu/versions.json", get(dpu_versions::list_json))
            .route(
                "/explored-endpoint.json",
                get(explored_endpoint::show_all_json),
            )
            .route("/explored-endpoint", get(explored_endpoint::show_html_all))
            .route(
                "/explored-endpoint/paired",
                get(explored_endpoint::show_html_paired),
            )
            .route(
                "/explored-endpoint/unpaired",
                get(explored_endpoint::show_html_unpaired),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}",
                get(explored_endpoint::detail),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/reexplore",
                post(explored_endpoint::re_explore),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/refresh",
                post(explored_endpoint::refresh_endpoint),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/power-control",
                post(explored_endpoint::power_control),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/bmc-reset",
                post(explored_endpoint::bmc_reset),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/clear-last-error",
                post(explored_endpoint::clear_last_exploration_error),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/pause-remediation",
                post(explored_endpoint::pause_remediation),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/machine-setup",
                post(explored_endpoint::machine_setup),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/set-dpu-first-boot-order",
                post(explored_endpoint::set_dpu_first_boot_order),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/restore-boot-interface",
                post(explored_endpoint::restore_boot_interface),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/clear-credentials",
                post(explored_endpoint::clear_bmc_credentials),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/delete",
                post(explored_endpoint::delete_endpoint),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/disable-secure-boot",
                post(explored_endpoint::disable_secure_boot),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/disable-lockdown",
                post(explored_endpoint::disable_lockdown),
            )
            .route(
                "/explored-endpoint/{endpoint_ip}/enable-lockdown",
                post(explored_endpoint::enable_lockdown),
            )
            .route("/firmware", get(firmware::show_html))
            .route("/firmware.json", get(firmware::show_json))
            .route("/host", get(machine::show_hosts_html))
            .route("/host.json", get(machine::show_hosts_json))
            .route("/ib-partition", get(ib_partition::show_html))
            .route("/ib-partition.json", get(ib_partition::show_all_json))
            .route("/ib-partition/{partition_id}", get(ib_partition::detail))
            .route("/ib-fabric", get(ib_fabric::show_html))
            .route("/ib-fabric.json", get(ib_fabric::show_all_json))
            .route("/instance", get(instance::show_html))
            .route("/instance.json", get(instance::show_all_json))
            .route("/instance/{instance_id}", get(instance::detail))
            .route("/compute-allocation", get(compute_allocation::show))
            .route("/compute-allocation", post(compute_allocation::create))
            .route(
                "/compute-allocation/{compute_allocation_id}",
                get(compute_allocation::show_detail),
            )
            .route(
                "/compute-allocation/{compute_allocation_id}",
                post(compute_allocation::update),
            )
            .route(
                "/compute-allocation/{compute_allocation_id}/delete",
                post(compute_allocation::delete),
            )
            .route("/instance-type", get(instance_type::show))
            .route(
                "/instance-type/{instance_type_id}",
                get(instance_type::show_detail),
            )
            .route("/interface", get(interface::show_html))
            .route("/interface.json", get(interface::show_all_json))
            .route("/interface/{interface_id}", get(interface::detail))
            .route("/ipam/dhcp", get(ipam::dhcp_html))
            .route("/ipam/dhcp.json", get(ipam::dhcp_json))
            .route("/ipam/dns", get(ipam::dns_html))
            .route("/ipam/underlay", get(ipam::underlay_html))
            .route(
                "/ipam/underlay/segment/{segment_id}",
                get(ipam::underlay_segment_html),
            )
            .route("/ipam/overlay", get(ipam::overlay_html))
            .route(
                "/ipam/overlay/prefix/{vpc_prefix_id}",
                get(ipam::overlay_prefix_html),
            )
            .route(
                "/ipam/overlay/segment/{segment_id}",
                get(ipam::overlay_segment_html),
            )
            .route("/machine", get(machine::show_all_html))
            .route("/machine.json", get(machine::show_all_json))
            .route("/machine/{machine_id}", get(machine::detail))
            .route(
                "/machine/{machine_id}/maintenance",
                post(machine::maintenance),
            )
            .route(
                "/machine/{machine_id}/quarantine",
                post(machine::quarantine),
            )
            .route("/machine/{machine_id}/sku", post(machine::sku))
            .route(
                "/machine/{machine_id}/boot-interface",
                post(machine::set_desired_boot_interface),
            )
            .route(
                "/machine/{machine_id}/boot-interface/reconcile",
                post(machine::reconcile_boot_interface),
            )
            .route("/machine/{machine_id}/health", get(health::machine_health))
            .route(
                "/machine/{machine_id}/health-history",
                get(health_history::show_machine_health_history),
            )
            .route(
                "/machine/{machine_id}/health-history.json",
                get(health_history::show_machine_health_history_json),
            )
            .route(
                "/machine/{machine_id}/state-history",
                get(state_history::show_machine_state_history),
            )
            .route(
                "/machine/{machine_id}/state-history.json",
                get(state_history::show_machine_state_history_json),
            )
            .route("/power-shelf", get(power_shelf::show_html))
            .route("/power-shelf.json", get(power_shelf::show_json))
            .route("/power-shelf/{power_shelf_id}", get(power_shelf::detail))
            .route(
                "/power-shelf/{power_shelf_id}/health",
                get(health::power_shelf_health),
            )
            .route(
                "/power-shelf/{power_shelf_id}/health/add-report",
                post(health::add_power_shelf_health_report),
            )
            .route(
                "/power-shelf/{power_shelf_id}/health/remove-report",
                post(health::remove_power_shelf_health_report),
            )
            .route(
                "/power-shelf/{power_shelf_id}/health-history",
                get(health_history::show_power_shelf_health_history),
            )
            .route(
                "/power-shelf/{power_shelf_id}/health-history.json",
                get(health_history::show_power_shelf_health_history_json),
            )
            .route(
                "/power-shelf/{power_shelf_id}/state-history",
                get(state_history::show_power_shelf_state_history),
            )
            .route(
                "/power-shelf/{power_shelf_id}/state-history.json",
                get(state_history::show_power_shelf_state_history_json),
            )
            .route("/rack", get(rack::show_html))
            .route("/rack.json", get(rack::show_json))
            .route("/rack/{rack_id}", get(rack::detail))
            .route("/rack/{rack_id}/health", get(health::rack_health))
            .route(
                "/rack/{rack_id}/health/add-report",
                post(health::add_rack_health_report),
            )
            .route(
                "/rack/{rack_id}/health/remove-report",
                post(health::remove_rack_health_report),
            )
            .route(
                "/rack/{rack_id}/health-history",
                get(health_history::show_rack_health_history),
            )
            .route(
                "/rack/{rack_id}/health-history.json",
                get(health_history::show_rack_health_history_json),
            )
            .route(
                "/rack/{rack_id}/state-history",
                get(state_history::show_rack_state_history),
            )
            .route(
                "/rack/{rack_id}/state-history.json",
                get(state_history::show_rack_state_history_json),
            )
            .route("/switch", get(switch::show_html))
            .route("/switch.json", get(switch::show_json))
            .route("/switch/{switch_id}", get(switch::detail))
            .route("/switch/{switch_id}/health", get(health::switch_health))
            .route(
                "/switch/{switch_id}/health/add-report",
                post(health::add_switch_health_report),
            )
            .route(
                "/switch/{switch_id}/health/remove-report",
                post(health::remove_switch_health_report),
            )
            .route(
                "/switch/{switch_id}/health-history",
                get(health_history::show_switch_health_history),
            )
            .route(
                "/switch/{switch_id}/health-history.json",
                get(health_history::show_switch_health_history_json),
            )
            .route(
                "/switch/{switch_id}/state-history",
                get(state_history::show_switch_state_history),
            )
            .route(
                "/switch/{switch_id}/state-history.json",
                get(state_history::show_switch_state_history_json),
            )
            .route(
                "/machine/{machine_id}/health/add-report",
                post(health::add_machine_health_report),
            )
            .route(
                "/machine/{machine_id}/health/remove-report",
                post(health::remove_machine_health_report),
            )
            .route(
                "/nvlink-domain/{domain_id}",
                get(health::nvlink_domain_detail),
            )
            .route(
                "/nvlink-domain/{domain_id}/health",
                get(health::nvlink_domain_health),
            )
            .route(
                "/nvlink-domain/{domain_id}/health/add-report",
                post(health::add_nvlink_domain_health_report),
            )
            .route(
                "/nvlink-domain/{domain_id}/health/remove-report",
                post(health::remove_nvlink_domain_health_report),
            )
            .route(
                "/machine/{machine_id}/attestation-results",
                get(attestation::show_attestation_results),
            )
            .route(
                "/attestation-summary",
                get(attestation::show_attestation_summary),
            )
            .route(
                "/machine/{machine_id}/attestation-submit-report-promotion",
                post(attestation::submit_report_promotion),
            )
            .route("/managed-host", get(managed_host::show_html))
            .route("/managed-host.json", get(managed_host::show_all_json))
            .route("/managed-host/{machine_id}", get(managed_host::detail))
            .route("/expected-machine", get(expected_machine::show_all_html))
            .route(
                "/expected-machine-definition.json",
                get(expected_machine::show_expected_machine_raw_json),
            )
            .route("/expected-rack", get(expected_rack::show_html))
            .route("/expected-rack.json", get(expected_rack::show_json))
            .route("/expected-switch", get(expected_switch::show_html))
            .route("/expected-switch.json", get(expected_switch::show_json))
            .route(
                "/expected-power-shelf",
                get(expected_power_shelf::show_html),
            )
            .route(
                "/expected-power-shelf.json",
                get(expected_power_shelf::show_json),
            )
            .route("/network-device", get(network_device::show_html))
            .route("/network-device.json", get(network_device::show_all_json))
            .route("/network-security-group", get(network_security_group::show))
            .route(
                "/network-security-group",
                post(network_security_group::create),
            )
            .route(
                "/network-security-group/{network_security_group_id}",
                get(network_security_group::show_detail),
            )
            .route(
                "/network-security-group/{network_security_group_id}",
                post(network_security_group::update),
            )
            .route(
                "/network-security-group/{network_security_group_id}/delete",
                post(network_security_group::delete),
            )
            .route("/ipxe-template", get(ipxe_template::show_html))
            .route("/ipxe-template.json", get(ipxe_template::show_all_json))
            .route("/ipxe-template/{name}", get(ipxe_template::detail))
            .route("/network-segment", get(network_segment::show_html))
            .route("/network-segment.json", get(network_segment::show_all_json))
            .route(
                "/network-segment/{segment_id}",
                get(network_segment::detail),
            )
            .route("/network-status", get(network_status::show_html))
            .route("/network-status.json", get(network_status::show_all_json))
            .route("/operating-system", get(operating_system::show_html))
            .route(
                "/operating-system.json",
                get(operating_system::show_all_json),
            )
            .route("/operating-system/{os_id}", get(operating_system::detail))
            .route("/nmxc-browser", get(nmxc_browser::query))
            .route(
                "/nvlink-domain",
                get(nvlink::show_nvlink_domain_health_html),
            )
            .route(
                "/nvlink-domain.json",
                get(nvlink::show_nvlink_domain_health_json),
            )
            .route(
                "/nvlink-partition",
                get(nvlink::show_nvlink_logical_partitions_html),
            )
            .route(
                "/nvlink-partition.json",
                get(nvlink::show_nvlink_logical_partitions_json),
            )
            .route("/nvlink-partition/{id}", get(nvlink::detail))
            .route("/spx-partition", get(spx_partition::show_html))
            .route("/spx-partition.json", get(spx_partition::show_all_json))
            .route("/resource-pool", get(resource_pool::show_html))
            .route("/resource-pool.json", get(resource_pool::show_all_json))
            .route("/vpc", get(vpc::show_html))
            .route("/vpc.json", get(vpc::show_all_json))
            .route("/vpc/{vpc_id}", get(vpc::detail))
            .route("/redfish-browser", get(redfish_browser::query))
            .route("/redfish-actions", get(redfish_actions::query))
            .route("/redfish-actions/create", post(redfish_actions::create))
            .route("/redfish-actions/approve", post(redfish_actions::approve))
            .route("/redfish-actions/apply", post(redfish_actions::apply))
            .route("/redfish-actions/cancel", post(redfish_actions::cancel))
            .route("/search", get(search::find))
            .route("/sku", get(sku::show_html))
            .route("/sku.json", get(sku::show_all_json))
            .route("/sku/{sku_id}", get(sku::detail))
            .route("/tenant", get(tenant::show_html))
            .route("/tenant.json", get(tenant::show_all_json))
            .route("/tenant/{organization_id}", get(tenant::detail))
            .route("/tenant_keyset", get(tenant_keyset::show_html))
            .route("/tenant_keyset.json", get(tenant_keyset::show_all_json))
            .route(
                "/tenant_keyset/{organization_id}/{keyset_id}",
                get(tenant_keyset::detail),
            )
            .route(&format!("/{AUTH_CALLBACK_ROOT}"), get(auth::callback))
            .route(
                "/machinevalidation/runs/{validation_id}",
                get(machine_validation::results),
            )
            .route(
                "/machinevalidation/resultdetails/{validation_id}/{test_id}",
                get(machine_validation::result_details),
            )
            .route(
                "/machinevalidation/tests",
                get(machine_validation::show_tests_html),
            )
            .route("/machinevalidation", get(machine_validation::runs))
            .route(
                "/machinevalidation/tests/{test_id}",
                get(machine_validation::show_tests_details_html),
            )
            .route(
                "/machinevalidation/external-config",
                get(machine_validation::external_configs),
            )
            .route("/ufm-browser", get(ufm_browser::query))
            .route("/logs", get(logs::page))
            .route("/logs/{source}/stream", get(logs::stream))
            .route("/logs/{source}/history", get(logs::history))
            // Admission is intentionally inside web authentication: OAuth
            // identity is established before fair scheduling classifies the
            // client. Axum runs the later web-auth layer first.
            .layer(axum::middleware::from_fn_with_state(
                admission,
                AdminAdmissionControl::middleware,
            ))
            .layer(axum::middleware::from_fn(web_auth_middleware_fn))
            .layer(CsrfLayer::new())
            .layer(Extension(Arc::new(web_auth)))
            .layer(Extension(oauth_extension_layer))
            .with_state(api),
    ))
}

async fn web_auth_middleware_fn(
    headers: HeaderMap,
    mut req: Request<AxumBody>,
    next: Next,
) -> Result<Response, StatusCode> {
    let oauth_extension_layer = match req.extensions().get::<Arc<WebAuth>>().map(AsRef::as_ref) {
        None => {
            tracing::error!("failed to find web authentication extension layer");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Some(WebAuth::None) => return Ok(next.run(req).await),
        Some(WebAuth::Basic {
            password,
            challenge,
        }) => {
            if basic_credentials_are_valid(&headers, password) {
                return Ok(next.run(req).await);
            }
            return Ok((StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, *challenge)]).into_response());
        }
        Some(WebAuth::OAuth2(layer)) => Arc::clone(layer),
    };

    // /auth-callback should pass through because that's
    // where microsoft will call back after the auth attempt.
    if req.uri().path().starts_with("/auth-callback") {
        return Ok(next.run(req).await);
    }

    let cookiejar: PrivateCookieJar = PrivateCookieJar::from_headers(
        &headers,
        oauth_extension_layer.private_cookiejar_key.clone(),
    );

    // Add an auth context (mocking grpc certificate auth context) if we have a unique name.
    let unique_name = cookiejar.get("unique_name");
    let group = cookiejar.get("group_name");
    if let Some((unique_name, group)) = unique_name.zip(group) {
        let extensions = req.extensions_mut();
        // Extend auth context if it exists.
        let auth_context: &mut AuthContext = extensions.get_or_insert_default();
        auth_context.principals.push(Principal::from_web_cookie(
            unique_name.value().to_string(),
            group.value().to_string(),
        ));
    }

    // If it exists, do we still want to accept it?
    if let Some(c) = cookiejar.get("sid").map(|cookie| cookie.value().to_owned())
        && let Ok(expiraton_timestamp) = c.parse::<u64>()
    {
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| {
                tracing::error!(error = %e, "failed to get system time for oauth2 expiration check");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .as_secs();

        // Still valid?  Let'em pass through to where they wanted
        // to go.
        if now_seconds < expiraton_timestamp {
            return Ok(next.run(req).await);
        }
    }

    // If not found or expired, we'll grab the oauth client and redirect to Azure for auth.

    // Generate a PKCE challenge.
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    // Generate the full authorization URL.
    let (auth_url, csrf_state) = oauth_extension_layer
        .client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("User.Read".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    // Store the pkce verifier so we can use it later
    // during code exchange when they hit our callback URL.
    // Using this with a cookie is a little weird, but it'll be encrypted.
    let pkce_cookie = Cookie::build(("pkce_verifier", pkce_verifier.secret().to_owned()))
        .path("/")
        .secure(true)
        .http_only(true)
        .build();

    // Store the csrf state so we can compare the state we get back from Azure
    // when they hit our callback URL.
    let csrf_cookie = Cookie::build(("csrf_state", csrf_state.secret().to_owned()))
        .path("/")
        .secure(true)
        .http_only(true)
        .build();

    // Store the page the user originally wanted so we can send them back after auth.
    let requested_page_cookie = Cookie::build((
        "requested_page",
        req.uri()
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or_else(|| req.uri().path())
            .to_string(),
    ))
    .path("/")
    .secure(true)
    .http_only(true)
    .build();

    Ok((
        cookiejar
            .remove(requested_page_cookie.clone())
            .remove(csrf_cookie.clone())
            .remove(pkce_cookie.clone())
            .add(requested_page_cookie)
            .add(csrf_cookie)
            .add(pkce_cookie),
        Redirect::to(auth_url.as_ref()),
    )
        .into_response())
}

fn basic_credentials_are_valid(headers: &HeaderMap, configured_password: &str) -> bool {
    let Some((scheme, encoded)) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
    else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("basic") || encoded.is_empty() {
        return false;
    }
    let Ok(credentials) = BASE64_STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(credentials) = String::from_utf8(credentials) else {
        return false;
    };
    let Some((username, password)) = credentials.split_once(':') else {
        return false;
    };

    username == BASIC_AUTH_USERNAME && password == configured_password
}

#[cfg(test)]
mod web_auth_tests {
    use axum::body::Body;
    use carbide_test_support::{Case, Outcome, check_cases, value_scenarios};
    use http::Request;
    use tower::ServiceExt as _;

    use super::*;

    #[test]
    fn auth_mode_parsing() {
        check_cases(
            [
                Case {
                    scenario: "basic",
                    input: "basic",
                    expect: Outcome::Yields(WebAuthMode::Basic),
                },
                Case {
                    scenario: "oauth2",
                    input: "oauth2",
                    expect: Outcome::Yields(WebAuthMode::OAuth2),
                },
                Case {
                    scenario: "none",
                    input: "none",
                    expect: Outcome::Yields(WebAuthMode::None),
                },
                Case {
                    scenario: "case insensitive",
                    input: "BASIC",
                    expect: Outcome::Yields(WebAuthMode::Basic),
                },
                Case {
                    scenario: "empty",
                    input: "",
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "unknown",
                    input: "saml",
                    expect: Outcome::Fails,
                },
            ],
            |value| value.parse::<WebAuthMode>().map_err(drop),
        );
        assert_eq!(WebAuthMode::default(), WebAuthMode::Basic);
    }

    #[test]
    fn basic_credentials() {
        value_scenarios!(run = |header: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(header) = header {
                headers.insert(AUTHORIZATION, header.parse().unwrap());
            }
            basic_credentials_are_valid(&headers, "correct-password")
        };
            "accepted" {
                Some("Basic YWRtaW46Y29ycmVjdC1wYXNzd29yZA==") => true,
                Some("basic YWRtaW46Y29ycmVjdC1wYXNzd29yZA==") => true,
            }
            "rejected" {
                Some("Basic dXNlcjpjb3JyZWN0LXBhc3N3b3Jk") => false,
                Some("Basic YWRtaW46d3Jvbmc=") => false,
                None => false,
                Some("Bearer token") => false,
                Some("Basic not-base64") => false,
                Some("Basic YWRtaW4=") => false,
            }
        );
        assert!(!basic_credentials_are_valid(&HeaderMap::new(), ""));
    }

    fn auth_test_router(auth: WebAuth) -> Router {
        Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(axum::middleware::from_fn(web_auth_middleware_fn))
            .layer(Extension(Arc::new(auth)))
    }

    #[tokio::test]
    async fn valid_basic_credentials_reach_the_ui() {
        let response = auth_test_router(WebAuth::basic(Some("secret".to_string())))
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn invalid_basic_credentials_receive_a_challenge() {
        let response = auth_test_router(WebAuth::basic(Some("secret".to_string())))
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[WWW_AUTHENTICATE], BASIC_AUTH_REALM);
    }

    #[tokio::test]
    async fn empty_basic_configuration_generates_a_usable_temporary_password() {
        let auth = WebAuth::basic(None);
        let WebAuth::Basic {
            password,
            challenge,
        } = &auth
        else {
            panic!("basic_auth must return Basic authentication");
        };
        assert_eq!(password.len(), 32);
        assert!(
            password
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        );
        assert_eq!(*challenge, TEMPORARY_BASIC_AUTH_REALM);

        let credentials = BASE64_STANDARD.encode(format!("{BASIC_AUTH_USERNAME}:{password}"));
        let response = auth_test_router(auth)
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(AUTHORIZATION, format!("Basic {credentials}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn temporary_password_challenge_points_to_the_logs() {
        let response = auth_test_router(WebAuth::basic(None))
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()[WWW_AUTHENTICATE],
            TEMPORARY_BASIC_AUTH_REALM
        );
    }

    #[tokio::test]
    async fn explicit_none_reaches_the_ui() {
        let response = auth_test_router(WebAuth::None)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct Index {
    version: &'static str,
    config: configuration::ConfigPageView,
    missing_default_credentials: Vec<DefaultCredential>,
}

impl Base for Index {}

async fn root(state: AxumState<Arc<Api>>) -> impl IntoResponse {
    let request = tonic::Request::new(forgerpc::DpuAgentUpgradePolicyRequest { new_policy: None });
    use forgerpc::AgentUpgradePolicy::*;
    let agent_upgrade_policy = match state
        .dpu_agent_upgrade_policy_action(request)
        .await
        .map(|response| response.into_inner())
        .map(|p| p.active_policy)
    {
        Ok(x) if x == Off as i32 => "Off",
        Ok(x) if x == UpOnly as i32 => "Upgrade only",
        Ok(x) if x == UpDown as i32 => "Upgrade and Downgrade",
        Ok(_) => "Unknown",
        Err(err) => {
            tracing::error!(error = %err, "dpu_agent_upgrade_policy_action");
            return (StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string()));
        }
    };

    let site_explorer_enabled = state
        .dynamic_settings
        .site_explorer_enabled
        .load(Ordering::Relaxed)
        .to_string();
    let create_machines = state
        .dynamic_settings
        .create_machines
        .load(Ordering::Relaxed)
        .to_string();
    let bmc_proxy = state
        .dynamic_settings
        .bmc_proxy
        .load()
        .as_ref()
        .clone()
        .map(|p| p.to_string());
    let tracing_enabled = state
        .dynamic_settings
        .tracing_enabled
        .load(Ordering::Relaxed)
        .to_string();

    let live_settings = configuration::LiveSettings {
        log_filter: state.log_filter_string(),
        site_explorer_enabled,
        create_machines,
        bmc_proxy,
        tracing_enabled,
        dpu_agent_upgrade_policy: agent_upgrade_policy.to_string(),
    };

    let effective = match serde_json::to_value(state.runtime_config.redacted()) {
        Ok(value) => value,
        Err(err) => {
            tracing::error!(error = %err, "serializing runtime config");
            return (StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string()));
        }
    };
    let config = configuration::build_config_page(
        carbide_api_core::cfg::CONFIG_REFERENCE_MD,
        &effective,
        &state.runtime_config.explicit_value_paths(),
        live_settings,
    );

    let index = Index {
        version: match carbide_version::v!(build_version) {
            "" => "dev",
            version => version,
        },
        config,
        missing_default_credentials: state.missing_default_credentials().await,
    };

    (StatusCode::OK, Html(index.render().unwrap()))
}

async fn static_data(
    _state: AxumState<Arc<Api>>,
    AxumPath(filename): AxumPath<String>,
) -> Response {
    match filename.as_str() {
        "sortable.js" => (
            StatusCode::OK,
            [(CONTENT_TYPE, "text/javascript")],
            SORTABLE_JS,
        )
            .into_response(),
        "sortable.css" => {
            (StatusCode::OK, [(CONTENT_TYPE, "text/css")], SORTABLE_CSS).into_response()
        }
        "carbide.css" => {
            (StatusCode::OK, [(CONTENT_TYPE, "text/css")], CARBIDE_CSS).into_response()
        }
        "tabs.js" => (StatusCode::OK, [(CONTENT_TYPE, "text/javascript")], TABS_JS).into_response(),
        "table_filter.js" => (
            StatusCode::OK,
            [(CONTENT_TYPE, "text/javascript")],
            TABLE_FILTER_JS,
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "No such file").into_response(),
    }
}

/// Creates a response that describes that `resource` was not found
fn not_found_response(resource: String) -> Response {
    (
        StatusCode::NOT_FOUND,
        Html(format!("Not found: {resource}")),
    )
        .into_response()
}

fn invalid_machine_id() -> String {
    "INVALID_MACHINE".to_string()
}

#[cfg(test)]
mod index_template_tests {
    use super::*;

    /// Renders the Configuration page template against the real reference doc
    /// to catch template/view-model mismatches without a running server.
    #[test]
    fn index_renders_config_page() {
        let effective = serde_json::json!({
            "listen": "[::]:1079",
            "asn": 65001,
            "attestation_enabled": false,
        });
        let mut explicit = std::collections::BTreeMap::new();
        explicit.insert("asn".to_string(), "site-config.toml".to_string());
        let live_settings = configuration::LiveSettings {
            log_filter: "info".to_string(),
            site_explorer_enabled: "true".to_string(),
            create_machines: "false".to_string(),
            bmc_proxy: None,
            tracing_enabled: "false".to_string(),
            dpu_agent_upgrade_policy: "Off".to_string(),
        };
        let config = configuration::build_config_page(
            carbide_api_core::cfg::CONFIG_REFERENCE_MD,
            &effective,
            &explicit,
            live_settings,
        );

        let index = Index {
            version: "test-version",
            config,
            missing_default_credentials: Vec::new(),
        };
        let html = index.render().expect("index template renders");

        // Tab navigation with the first group tab active.
        assert!(html.contains(r#"id="tab-networking""#));
        // The overridden option shows its value and source tag.
        assert!(html.contains("65001"));
        assert!(html.contains("site-config.toml"));
        // Runtime settings are folded in and tagged.
        assert!(html.contains(r#"class="config-runtime""#));
        assert!(html.contains("log_filter"));
        // Catalog rendering is present.
        assert!(html.contains("attestation_enabled"));
        assert!(html.contains("site_explorer"));
    }
}
