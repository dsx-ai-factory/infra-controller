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

//! The NVUE REST API of a switch's NVOS, served at the switch's NVOS
//! management address.
//!
//! NVOS exposes its `nv show` state over HTTPS as the NVUE REST API: a `GET`
//! of `/nvue_v1/<path>` answers the JSON of that `nv show` tree, authenticated
//! with HTTP Basic auth as an NVOS user. This module serves the read-only
//! subset NICo consumes, which hardware health polls and the BMC proxy
//! forwards, in the shape an NVLink switch tray running NVOS 25.02.4463
//! answers. Configuration writes, revisions and `nv config apply` are not
//! modeled.
//!
//! # Layout
//!
//! One module per `nv show` tree the mock answers for: [`system`],
//! [`platform`], [`interface`], [`cluster`] and [`sdn`]. [`account`] holds the
//! NVOS users Basic auth is checked against. A hardware profile supplies a
//! [`Config`]; [`NvueState`] holds the values simulation may change. The
//! boundary is the one `redfish` keeps: handlers consume configured state and
//! never inspect a hardware type.
//!
//! # Query parameters
//!
//! On NVOS `rev` selects a configuration revision; the mock serves
//! operational values for every `rev`, where NVOS answers `{}` for an
//! operational-only tree asked for `applied`. `filter_`, `include` and `omit`
//! are accepted and ignored. NVOS 25.02 ignores `filter_` on
//! `/nvue_v1/interface` as well, so every interface type is listed whatever
//! the filter; ignoring `include` and `omit` means every response carries
//! every field, which a client asking for a subset tolerates. Unknown
//! parameters are ignored.
//!
//! # Errors
//!
//! NVUE sits behind nginx, which answers a request without valid Basic
//! credentials with its own `text/html` 401 page and
//! `WWW-Authenticate: Basic realm="nvueapi"`, and `HEAD` and `OPTIONS` with
//! its 403 page. Past nginx, errors are `application/problem+json` objects as
//! NVUE's Connexion server sends them: a `detail`, the numeric `status`, a
//! `title`, and `"type": "about:blank"`. An unknown path is 404, and a method
//! other than `GET` on a known path is 405 with `Allow: HEAD, OPTIONS, GET`.

mod account;
mod cluster;
mod interface;
mod platform;
mod sdn;
mod system;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{ALLOW, CONTENT_TYPE, SERVER, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;
use axum_extra::headers::authorization::Basic;
use axum_extra::headers::{Authorization, HeaderMapExt};
use chrono::{DateTime, Utc};
use mac_address::MacAddress;
use serde_json::json;

pub(crate) use self::account::Account;
use self::account::Accounts;
use crate::HostMachineInfo;
use crate::injection::InjectionStore;

/// The realm the switch's nginx names when it asks for credentials.
const WWW_AUTHENTICATE_VALUE: HeaderValue = HeaderValue::from_static("Basic realm=\"nvueapi\"");

/// What a switch profile knows about its NVOS: identity, inventory and
/// initial values, from which [`NvueState`] is built once.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// The hostname `nv show system` reports.
    pub(crate) hostname: String,
    /// The system type `nv show platform` reports, such as `N5700_LD`.
    pub(crate) system_type: &'static str,
    /// The switch's part number.
    pub(crate) part_number: &'static str,
    /// The NVOS release, such as `25.02.4463`.
    pub(crate) product_release: &'static str,
    /// The switch serial number.
    pub(crate) serial_number: String,
    /// The MAC address of `eth0`, the NVOS management interface, when the
    /// simulated switch has one.
    pub(crate) management_mac: Option<MacAddress>,
    /// The NVOS administrator Basic auth is checked against.
    pub(crate) admin: Account,
    /// Fans by name, such as `FAN1/1`.
    pub(crate) fans: Vec<FanConfig>,
    /// Temperature sensors by name, such as `ASIC1`.
    pub(crate) temperatures: Vec<TemperatureConfig>,
    /// Leak sensors by name, such as `LEAKAGE-1`; every one starts dry.
    pub(crate) leak_sensors: Vec<String>,
    /// How many NVLink ports the switch exposes, as `acp1` to `acp<n>`.
    pub(crate) nvl_ports: u32,
    /// The cluster applications `nv show cluster apps` lists.
    pub(crate) cluster_apps: Vec<ClusterAppConfig>,
    /// The SDN partitions `nv show sdn partition` lists.
    pub(crate) partitions: Vec<PartitionConfig>,
}

/// One fan of `nv show platform environment fan`.
#[derive(Clone, Debug)]
pub(crate) struct FanConfig {
    pub(crate) name: String,
    /// The airflow direction, `F2B` (front to back) or `B2F`.
    pub(crate) direction: &'static str,
    pub(crate) max_speed_rpm: u32,
    pub(crate) min_speed_rpm: u32,
    pub(crate) current_speed_rpm: u32,
}

/// One temperature sensor of `nv show platform environment temperature`, in
/// degrees Celsius. NVOS reports only the thresholds a sensor has, so each is
/// optional.
#[derive(Clone, Debug)]
pub(crate) struct TemperatureConfig {
    pub(crate) name: String,
    pub(crate) current: f32,
    pub(crate) min: Option<f32>,
    pub(crate) max: Option<f32>,
    pub(crate) crit: Option<f32>,
}

/// One application of `nv show cluster apps`.
#[derive(Clone, Debug)]
pub(crate) struct ClusterAppConfig {
    pub(crate) name: &'static str,
    pub(crate) app_id: &'static str,
    pub(crate) app_version: &'static str,
    pub(crate) components_version: &'static str,
    pub(crate) capabilities: &'static str,
    /// Free-form detail beside the status; the NMX controller reports its
    /// control plane state here.
    pub(crate) addition_info: &'static str,
}

/// One partition of `nv show sdn partition`.
#[derive(Clone, Debug)]
pub(crate) struct PartitionConfig {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) num_gpus: u32,
    pub(crate) partition_type: &'static str,
    pub(crate) resiliency_mode: &'static str,
}

/// What a leak sensor reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeakState {
    /// The sensor is dry.
    Ok,
    /// The sensor detects liquid.
    Leak,
}

impl LeakState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Leak => "leak",
        }
    }
}

/// The overall status `nv show system health` reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemHealthStatus {
    /// Every monitored component is healthy.
    Ok,
    /// At least one component reports an issue.
    NotOk,
}

impl SystemHealthStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::NotOk => "Not OK",
        }
    }

    fn led(self) -> &'static str {
        match self {
            Self::Ok => "green",
            Self::NotOk => "amber",
        }
    }
}

/// A sensor name the configured switch does not have.
#[derive(Debug, thiserror::Error)]
#[error("the switch has no leak sensor named {0}")]
pub struct UnknownLeakSensor(String);

/// A username the NVOS has no account for.
#[derive(Debug, thiserror::Error)]
#[error("NVOS has no user named {0}")]
pub struct UnknownUser(String);

/// What the mock serves for one switch's NVOS: the profile's configuration
/// and the values simulation may change while it runs.
#[derive(Debug)]
pub struct NvueState {
    config: Config,
    accounts: Accounts,
    /// When the simulated NVOS booted: the state's creation.
    booted: Instant,
    booted_at: DateTime<Utc>,
    health: Mutex<SystemHealth>,
    leaks: Mutex<BTreeMap<String, LeakState>>,
}

/// `nv show system health`: the status, and the components with an issue.
#[derive(Clone, Debug)]
struct SystemHealth {
    status: SystemHealthStatus,
    issues: BTreeMap<String, String>,
}

impl NvueState {
    pub(crate) fn from_config(config: Config) -> Self {
        let leaks = config
            .leak_sensors
            .iter()
            .map(|sensor| (sensor.clone(), LeakState::Ok))
            .collect();
        Self {
            accounts: Accounts::new(config.admin.clone()),
            booted: Instant::now(),
            booted_at: Utc::now(),
            health: Mutex::new(SystemHealth {
                status: SystemHealthStatus::Ok,
                issues: BTreeMap::new(),
            }),
            leaks: Mutex::new(leaks),
            config,
        }
    }

    /// The administrator's current username and password, as a client must
    /// present them.
    pub fn admin_credentials(&self) -> (String, String) {
        self.accounts.admin_credentials()
    }

    /// Changes `username`'s password, as a completed NVOS password rotation
    /// does on the switch.
    pub fn set_password(&self, username: &str, new_password: &str) -> Result<(), UnknownUser> {
        self.accounts.set_password(username, new_password)
    }

    /// Sets what the leak sensor `sensor` reports.
    pub fn set_leak_state(&self, sensor: &str, state: LeakState) -> Result<(), UnknownLeakSensor> {
        let mut leaks = lock(&self.leaks);
        let Some(current) = leaks.get_mut(sensor) else {
            return Err(UnknownLeakSensor(sensor.to_string()));
        };
        *current = state;
        Ok(())
    }

    /// Sets the overall health and the components `nv show system health`
    /// lists with an issue. An empty `issues` with [`SystemHealthStatus::NotOk`]
    /// is allowed: NVOS reports the status and the issue table independently.
    pub fn set_system_health(
        &self,
        status: SystemHealthStatus,
        issues: impl IntoIterator<Item = (String, String)>,
    ) {
        *lock(&self.health) = SystemHealth {
            status,
            issues: issues.into_iter().collect(),
        };
    }

    fn health(&self) -> SystemHealth {
        lock(&self.health).clone()
    }

    fn leaks(&self) -> BTreeMap<String, LeakState> {
        lock(&self.leaks).clone()
    }

    fn uptime(&self) -> Duration {
        self.booted.elapsed()
    }

    /// Whether any leak sensor is wet.
    fn leaking(&self) -> bool {
        lock(&self.leaks)
            .values()
            .any(|leak| *leak == LeakState::Leak)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("an NVUE state mutex must not be poisoned")
}

/// The NVUE REST API of `host`'s NVOS, when its profile has one: the router
/// to serve at the switch's NVOS address, and the state behind it. Rules in
/// `injection` apply to its requests and are managed through the same
/// control endpoints as a BMC's.
pub fn nvue_router(
    host: &HostMachineInfo,
    injection: Arc<InjectionStore>,
) -> Option<(Router, Arc<NvueState>)> {
    let state = Arc::new(NvueState::from_config(host.nvue_config()?));
    Some((router(state.clone(), injection), state))
}

/// The router serving `state`, with the nginx front (method gate and Basic
/// auth) ahead of every route and fault injection ahead of that.
fn router(state: Arc<NvueState>, injection: Arc<InjectionStore>) -> Router {
    let routes = Router::new()
        .route("/nvue_v1/system", get(system::show))
        .route("/nvue_v1/system/health", get(system::health))
        .route("/nvue_v1/system/reboot/reason", get(system::reboot_reason))
        .route("/nvue_v1/system/version", get(system::version))
        .route("/nvue_v1/platform", get(platform::show))
        .route("/nvue_v1/platform/environment", get(platform::environment))
        .route("/nvue_v1/platform/environment/fan", get(platform::fans))
        .route(
            "/nvue_v1/platform/environment/led",
            get(platform::leds_tree),
        )
        .route(
            "/nvue_v1/platform/environment/leakage",
            get(platform::leakage),
        )
        .route(
            "/nvue_v1/platform/environment/temperature",
            get(platform::temperatures),
        )
        .route("/nvue_v1/interface", get(interface::list))
        .route("/nvue_v1/cluster/apps", get(cluster::apps))
        .route("/nvue_v1/sdn/partition", get(sdn::partitions))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            nginx_front,
        ))
        .with_state(state);
    carbide_axum_utils::injection::injection_router(routes, injection.clone())
        .merge(carbide_axum_utils::injection::management_router(injection))
}

/// What the switch's nginx does before NVUE sees a request: refuses `HEAD`
/// and `OPTIONS`, and asks for Basic credentials of an NVOS user. Any
/// `Authorization` header that is not decodable Basic credentials is as good
/// as none.
async fn nginx_front(
    State(state): State<Arc<NvueState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if matches!(*request.method(), Method::HEAD | Method::OPTIONS) {
        return nginx_page(StatusCode::FORBIDDEN, "403 Forbidden");
    }
    let authorized = request
        .headers()
        .typed_get::<Authorization<Basic>>()
        .is_some_and(|basic| {
            state
                .accounts
                .is_authorized(basic.username(), basic.password())
        });
    if !authorized {
        let mut response = nginx_page(StatusCode::UNAUTHORIZED, "401 Authorization Required");
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, WWW_AUTHENTICATE_VALUE);
        return response;
    }
    next.run(request).await
}

/// nginx's own error page for `status`, titled `title`.
fn nginx_page(status: StatusCode, title: &str) -> Response {
    let body = format!(
        "<html>\r\n<head><title>{title}</title></head>\r\n<body>\r\n<center><h1>{title}</h1></center>\r\n<hr><center>nginx</center>\r\n</body>\r\n</html>\r\n"
    );
    Response::builder()
        .status(status)
        .header(SERVER, HeaderValue::from_static("nginx"))
        .header(CONTENT_TYPE, HeaderValue::from_static("text/html"))
        .body(Body::from(body))
        .expect("an nginx page builds")
}

async fn not_found() -> Response {
    problem(
        StatusCode::NOT_FOUND,
        "The requested URL was not found on the server. If you entered the URL manually please check your spelling and try again.",
    )
}

async fn method_not_allowed() -> Response {
    let mut response = problem(
        StatusCode::METHOD_NOT_ALLOWED,
        "The method is not allowed for the requested URL.",
    );
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static("HEAD, OPTIONS, GET"));
    response
}

/// An `application/problem+json` error as NVUE sends them.
fn problem(status: StatusCode, detail: &str) -> Response {
    let body = json!({
        "detail": detail,
        "status": status.as_u16(),
        "title": status.canonical_reason().unwrap_or("Error"),
        "type": "about:blank",
    });
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/problem+json")
        .body(Body::from(body.to_string()))
        .expect("a problem response builds")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, Bytes};
    use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
    use axum_extra::headers::{Authorization, HeaderMapExt};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{LeakState, NvueState, SystemHealthStatus, nvue_router};
    use crate::injection::InjectionStore;
    use crate::test_support::host_info;
    use crate::{
        DUMMY_FACTORY_PASSWORD, DUMMY_NVOS_USERNAME, HardwareType, HostMachineInfo, MachineInfo,
    };

    /// A host of `hardware_type`.
    fn host(hardware_type: HardwareType) -> HostMachineInfo {
        match host_info(hardware_type) {
            MachineInfo::Host(host) => host,
            MachineInfo::Dpu(_) => unreachable!("host_info builds a host"),
        }
    }

    fn switch() -> (axum::Router, Arc<NvueState>) {
        nvue_router(
            &host(HardwareType::NvidiaSwitchN5700Ld),
            Arc::new(InjectionStore::new()),
        )
        .expect("a switch profile serves NVUE")
    }

    /// The `Authorization` value for `username` and `password`.
    fn basic(username: &str, password: &str) -> HeaderValue {
        let mut headers = HeaderMap::new();
        headers.typed_insert(Authorization::basic(username, password));
        headers[header::AUTHORIZATION].clone()
    }

    fn admin() -> HeaderValue {
        basic(DUMMY_NVOS_USERNAME, DUMMY_FACTORY_PASSWORD)
    }

    /// `method` on `path`, with `authorization` when given: the status, the
    /// response headers and the raw body.
    async fn exchange(
        router: &axum::Router,
        method: Method,
        path: &str,
        authorization: Option<&HeaderValue>,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization.clone());
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).expect("request builds"))
            .await
            .expect("router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        (status, headers, body)
    }

    /// `method` on `path` as the administrator: the status and the JSON body.
    async fn exchange_json(
        router: &axum::Router,
        method: Method,
        path: &str,
    ) -> (StatusCode, Value) {
        let (status, _, body) = exchange(router, method, path, Some(&admin())).await;
        (status, serde_json::from_slice(&body).expect("JSON body"))
    }

    /// Every route is behind Basic auth as the configured NVOS admin, whose
    /// password a rotation changes; a refusal is nginx's, with the realm it
    /// names.
    #[tokio::test]
    async fn routes_require_the_current_nvos_credentials() {
        let (router, state) = switch();
        let rotated = "rotated-password";
        for (scenario, (authorization, after_rotation), expected) in [
            (
                "no credentials",
                (None, false),
                (StatusCode::UNAUTHORIZED, true),
            ),
            (
                "wrong password",
                (Some(basic(DUMMY_NVOS_USERNAME, "nope")), false),
                (StatusCode::UNAUTHORIZED, true),
            ),
            (
                "credentials that are not Basic",
                (Some(HeaderValue::from_static("Bearer xyz")), false),
                (StatusCode::UNAUTHORIZED, true),
            ),
            (
                "the factory password",
                (Some(admin()), false),
                (StatusCode::OK, false),
            ),
            (
                "the factory password after a rotation",
                (Some(admin()), true),
                (StatusCode::UNAUTHORIZED, true),
            ),
            (
                "the rotated password",
                (Some(basic(DUMMY_NVOS_USERNAME, rotated)), true),
                (StatusCode::OK, false),
            ),
        ] {
            let actual = {
                let router = router.clone();
                let state = state.clone();
                {
                    let password = if after_rotation {
                        rotated
                    } else {
                        DUMMY_FACTORY_PASSWORD
                    };
                    state
                        .set_password(DUMMY_NVOS_USERNAME, password)
                        .expect("the admin exists");
                    let (status, headers, _) = exchange(
                        &router,
                        Method::GET,
                        "/nvue_v1/system/health",
                        authorization.as_ref(),
                    )
                    .await;
                    let realm = headers
                        .get(header::WWW_AUTHENTICATE)
                        .map(|value| value == "Basic realm=\"nvueapi\"");
                    (status, realm == Some(true))
                }
            };
            assert_eq!(actual, expected, "{scenario}");
        }
        assert_eq!(
            state.admin_credentials(),
            (DUMMY_NVOS_USERNAME.to_string(), rotated.to_string())
        );
        assert!(state.set_password("nobody", "x").is_err());

        // nginx refuses HEAD even with credentials.
        let (status, headers, _) = exchange(
            &router,
            Method::HEAD,
            "/nvue_v1/system/health",
            Some(&basic(DUMMY_NVOS_USERNAME, rotated)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(headers[header::CONTENT_TYPE], "text/html");
    }

    /// Each served tree answers the shape the switch does; the rest of the
    /// API is 404 and writes are 405.
    #[tokio::test]
    async fn trees_answer_their_nv_show_shape() {
        let (router, _state) = switch();
        for (scenario, (method, path, pointer), expected) in [
            (
                "system identity",
                (
                    Method::GET,
                    "/nvue_v1/system?rev=operational",
                    "/version/product-release",
                ),
                (StatusCode::OK, json!("25.02.4463")),
            ),
            (
                "system version",
                (Method::GET, "/nvue_v1/system/version", "/image/build-id"),
                (StatusCode::OK, json!("nvos-25.02.4463")),
            ),
            (
                "platform identity",
                (Method::GET, "/nvue_v1/platform", "/system-type"),
                (StatusCode::OK, json!("N5700_LD")),
            ),
            (
                "system health",
                (Method::GET, "/nvue_v1/system/health", "/status"),
                (StatusCode::OK, json!("OK")),
            ),
            (
                "reboot reason",
                (Method::GET, "/nvue_v1/system/reboot/reason", "/reason"),
                (StatusCode::OK, json!("reboot")),
            ),
            (
                "environment lists every sensor with its type",
                (
                    Method::GET,
                    "/nvue_v1/platform/environment",
                    "/FAN1~11/type",
                ),
                (StatusCode::OK, json!("fan")),
            ),
            (
                "environment LEDs",
                (
                    Method::GET,
                    "/nvue_v1/platform/environment",
                    "/STATUS/state",
                ),
                (StatusCode::OK, json!("green")),
            ),
            (
                "LED colors",
                (
                    Method::GET,
                    "/nvue_v1/platform/environment/led",
                    "/UID/color",
                ),
                (StatusCode::OK, json!("off")),
            ),
            (
                "fans",
                (
                    Method::GET,
                    "/nvue_v1/platform/environment/fan",
                    "/FAN1~11/current-speed",
                ),
                (StatusCode::OK, json!("10096")),
            ),
            (
                "a temperature sensor carries only the thresholds it has",
                (
                    Method::GET,
                    "/nvue_v1/platform/environment/temperature",
                    "/Ambient-MNG-Temp",
                ),
                (StatusCode::OK, json!({"current": "23.50", "state": "ok"})),
            ),
            (
                "leak sensors",
                (
                    Method::GET,
                    "/nvue_v1/platform/environment/leakage",
                    "/LEAKAGE-1/state",
                ),
                (StatusCode::OK, json!("ok")),
            ),
            (
                "NVLink interfaces",
                (
                    Method::GET,
                    "/nvue_v1/interface?rev=operational&filter_=type%3Dnvl&include=%2F*%2Ftype",
                    "/acp1/link/state",
                ),
                (StatusCode::OK, json!({"up": {}})),
            ),
            (
                "the type filter is ignored, as NVOS ignores it",
                (
                    Method::GET,
                    "/nvue_v1/interface?filter_=type%3Dnvl",
                    "/eth0/type",
                ),
                (StatusCode::OK, json!("eth")),
            ),
            (
                "cluster applications",
                (
                    Method::GET,
                    "/nvue_v1/cluster/apps",
                    "/nmx-controller/addition-info",
                ),
                (StatusCode::OK, json!("CONTROL_PLANE_STATE_CONFIGURED")),
            ),
            (
                "partitions",
                (Method::GET, "/nvue_v1/sdn/partition", "/32766/health"),
                (StatusCode::OK, json!("healthy")),
            ),
            (
                "an unmodeled tree",
                (Method::GET, "/nvue_v1/vrf", "/status"),
                (StatusCode::NOT_FOUND, json!(404)),
            ),
            (
                "a write",
                (Method::PATCH, "/nvue_v1/system", "/status"),
                (StatusCode::METHOD_NOT_ALLOWED, json!(405)),
            ),
        ] {
            let actual = {
                let router = router.clone();
                {
                    let (status, body) = exchange_json(&router, method, path).await;
                    // A JSON pointer spells the `/` of `FAN1/1` as `~1`.
                    (
                        status,
                        body.pointer(pointer).cloned().unwrap_or(Value::Null),
                    )
                }
            };
            assert_eq!(actual, expected, "{scenario}");
        }
    }

    /// Simulation changes what the sensors, the LEDs and the health tree
    /// report.
    #[tokio::test]
    async fn simulated_faults_show_in_their_trees() {
        let (router, state) = switch();
        state
            .set_leak_state("LEAKAGE-2", LeakState::Leak)
            .expect("LEAKAGE-2 exists");
        assert!(state.set_leak_state("LEAKAGE-9", LeakState::Leak).is_err());
        state.set_system_health(
            SystemHealthStatus::NotOk,
            [("LEAKAGE-2".to_string(), "leak detected".to_string())],
        );

        let (_, leakage) = exchange_json(
            &router,
            Method::GET,
            "/nvue_v1/platform/environment/leakage",
        )
        .await;
        assert_eq!(leakage["LEAKAGE-1"]["state"], json!("ok"));
        assert_eq!(leakage["LEAKAGE-2"]["state"], json!("leak"));

        let (_, environment) =
            exchange_json(&router, Method::GET, "/nvue_v1/platform/environment").await;
        assert_eq!(environment["STATUS"]["state"], json!("amber"));

        let (_, health) = exchange_json(&router, Method::GET, "/nvue_v1/system/health").await;
        assert_eq!(health["status"], json!("Not OK"));
        assert_eq!(health["status-led"], json!("amber"));
        assert_eq!(
            health["issues"]["LEAKAGE-2"]["issue"],
            json!("leak detected")
        );
    }

    /// Only switch profiles have an NVOS to serve.
    #[test]
    fn only_switch_profiles_serve_nvue() {
        for (scenario, hardware_type, expected) in [
            (
                "an NVLink switch tray",
                HardwareType::NvidiaSwitchN5700Ld,
                true,
            ),
            (
                "the other NVLink switch tray",
                HardwareType::NvidiaSwitchNd5200Ld,
                true,
            ),
            ("a compute tray", HardwareType::WiwynnGB200Nvl, false),
            ("a power shelf", HardwareType::DeltaPowerShelf, false),
        ] {
            let actual =
                { nvue_router(&host(hardware_type), Arc::new(InjectionStore::new())).is_some() };
            assert_eq!(actual, expected, "{scenario}");
        }
    }
}
