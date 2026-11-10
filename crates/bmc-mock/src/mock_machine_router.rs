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
use std::sync::Arc;

use axum::Router;

use crate::auth_router::Authorizer;
use crate::bmc_state::BmcState;
use crate::injection::InjectionStore;
use crate::redfish::manager::ManagerState;
use crate::{
    Callbacks, EventServiceConfig, MachineInfo, VirtualMediaDeviceConfig, auth_router,
    middleware_router, redfish,
};

/// Caller control over the hardware profile's EventService.
#[derive(Clone, Debug, Default)]
pub enum EventServiceOverride {
    /// Serve the profile's configuration. Every current profile enables the
    /// service with default limits.
    #[default]
    Profile,
    /// Omit the service, its service-root link, and its routes even when the
    /// profile enables it.
    Disabled,
    /// Replace the profile's limits. Cannot enable a service the profile omits.
    Limits(EventServiceConfig),
}

#[derive(Debug, Default)]
pub struct MachineRouterOptions {
    /// EventService selection. `Profile` (the default) serves the hardware
    /// profile's configuration, `Disabled` omits the service and its routes,
    /// and `Limits` replaces the profile's limits when the profile enables it.
    pub event_service: EventServiceOverride,
    pub virtual_media_devices: Option<Vec<VirtualMediaDeviceConfig>>,
    /// Enables the BMC self-reset simulation: after `Manager.Reset` the
    /// mock answers 503 to everything for this duration (per-platform,
    /// from the machine's resolved `LifecycleTimings::bmc_reset`).
    /// `None` and `Some(Duration::ZERO)` disable the offline window.
    /// An enabled event service still closes streams and clears history on reset;
    /// with both features disabled, resets remain no-ops.
    pub bmc_reset_duration: Option<std::time::Duration>,
    /// How long a firmware upload's Redfish task stays `Running` before it
    /// reports `Completed` (per-platform, from the machine's resolved
    /// `LifecycleTimings::firmware_upgrade`). `None` keeps the update
    /// service's built-in 2 s. The jitter added on top is capped at the
    /// duration, so a zero duration completes tasks immediately.
    pub firmware_upgrade_duration: Option<std::time::Duration>,
}

trait AddRoutes {
    fn add_routes(self, f: impl FnOnce(Self) -> Self) -> Self
    where
        Self: Sized;
}

impl<S: Clone + Send + Sync + 'static> AddRoutes for Router<S> {
    fn add_routes(self, f: impl FnOnce(Self) -> Self) -> Self {
        f(self)
    }
}

/// Return an axum::Router that mocks various redfish calls to match
/// the provided MachineInfo.
pub fn machine_router<C: Callbacks>(
    machine_info: &MachineInfo,
    callbacks: Arc<C>,
    mat_host_id: String,
    redfish_auth: bool,
    options: MachineRouterOptions,
) -> (Router, BmcState<C>) {
    machine_router_inner(
        machine_info,
        callbacks,
        mat_host_id,
        redfish_auth,
        Arc::new(InjectionStore::new()),
        options,
        machine_info.event_service_config(),
        |router, _| router,
    )
}

/// Return a machine router backed by a caller-provided injection store.
pub fn machine_router_with_injection_store<C: Callbacks>(
    machine_info: &MachineInfo,
    callbacks: Arc<C>,
    mat_host_id: String,
    redfish_auth: bool,
    injection: Arc<InjectionStore>,
    options: MachineRouterOptions,
) -> (Router, BmcState<C>) {
    machine_router_inner(
        machine_info,
        callbacks,
        mat_host_id,
        redfish_auth,
        injection,
        options,
        machine_info.event_service_config(),
        |router, _| router,
    )
}

/// Return a machine router with routes and middleware added by the caller.
///
/// `extend` receives the profile's routes, before the router is bound to its
/// state, and the state it will be bound to. Routes it adds are served like the
/// profile's own: behind Redfish authentication, the BMC outage window, fault
/// injection, query handling and the Redfish error envelope. A layer it adds
/// wraps the routes registered before it, including the profile's, so it may
/// decorate their responses. Registering a method and path the profile already
/// serves panics, as for any axum router.
pub fn machine_router_with_extension<C: Callbacks>(
    machine_info: &MachineInfo,
    callbacks: Arc<C>,
    mat_host_id: String,
    redfish_auth: bool,
    options: MachineRouterOptions,
    extend: impl FnOnce(Router<BmcState<C>>, &BmcState<C>) -> Router<BmcState<C>>,
) -> (Router, BmcState<C>) {
    machine_router_inner(
        machine_info,
        callbacks,
        mat_host_id,
        redfish_auth,
        Arc::new(InjectionStore::new()),
        options,
        machine_info.event_service_config(),
        extend,
    )
}

#[allow(clippy::too_many_arguments)]
fn machine_router_inner<C: Callbacks>(
    machine_info: &MachineInfo,
    callbacks: Arc<C>,
    mat_host_id: String,
    redfish_auth: bool,
    injection: Arc<InjectionStore>,
    options: MachineRouterOptions,
    profile_event_service: Option<EventServiceConfig>,
    extend: impl FnOnce(Router<BmcState<C>>, &BmcState<C>) -> Router<BmcState<C>>,
) -> (Router, BmcState<C>) {
    let system_config = machine_info.system_config(callbacks.clone());
    let chassis_config = machine_info.chassis_config();
    let mut update_service_config = machine_info.update_service_config();
    if let Some(delay) = options.firmware_upgrade_duration {
        update_service_config.task_completion_delay = delay;
        update_service_config.task_completion_jitter =
            crate::redfish::update_service::DEFAULT_TASK_COMPLETION_JITTER.min(delay);
    }
    let service_root_config = machine_info.service_root_config();
    let oem_state = machine_info.oem_state();
    let factory_default_account = machine_info.factory_default_account();
    let event_service_config = match &options.event_service {
        EventServiceOverride::Profile => profile_event_service,
        EventServiceOverride::Disabled => None,
        EventServiceOverride::Limits(limits) => profile_event_service.map(|_| limits.clone()),
    };
    let router = Router::new()
        .add_routes(crate::redfish::service_root::add_routes)
        .add_routes(|router| {
            if event_service_config.is_some() {
                crate::event_controls::add_routes(crate::redfish::event_service::add_routes(router))
            } else {
                router
            }
        })
        .add_routes(crate::redfish::chassis::add_routes)
        .add_routes(crate::redfish::component_integrity::add_routes)
        .add_routes(crate::redfish::manager::add_routes)
        .add_routes(crate::redfish::update_service::add_routes)
        .add_routes(crate::redfish::task_service::add_routes)
        .add_routes(crate::redfish::telemetry_service::add_routes)
        .add_routes(crate::redfish::account_service::add_routes)
        .add_routes(crate::redfish::session_service::add_routes)
        .add_routes(crate::redfish::virtual_media::add_routes);
    let router = match machine_info {
        MachineInfo::Dpu(_) => {
            router.add_routes(crate::redfish::oem::nvidia::bluefield::add_routes)
        }
        MachineInfo::Host(_) => router
            .add_routes(crate::redfish::oem::nvidia::chassis::add_routes)
            .add_routes(crate::redfish::oem::dell::idrac::add_routes)
            .add_routes(crate::redfish::oem::supermicro::manager::add_routes),
    };
    let manager = Arc::new(ManagerState::new(&machine_info.manager_config()));
    let system_state = Arc::new(crate::redfish::computer_system::SystemState::from_config(
        system_config,
        &options,
    ));
    let chassis_state = Arc::new(crate::redfish::chassis::ChassisState::from_config(
        chassis_config,
    ));
    let update_service_state = Arc::new(
        crate::redfish::update_service::UpdateServiceState::from_config(update_service_config),
    );
    // Desired firmware versions arrive via UpdateServiceConfig.pending_upgrades and
    // are stored in UpdateServiceState at construction time via from_config().
    // No separate pre-staging call is needed here.
    let account_service_state = Arc::new(
        crate::redfish::account_service::AccountServiceState::new(factory_default_account),
    );
    let session_service_state =
        Arc::new(crate::redfish::session_service::SessionServiceState::new());
    let availability = options
        .bmc_reset_duration
        .filter(|d| !d.is_zero())
        .map(|d| Arc::new(crate::availability::BmcAvailabilityState::new(d)));
    let component_integrities = machine_info.component_integrity_config();
    let service_root_state = Arc::new(redfish::service_root::ServiceRootState::from_config(
        service_root_config,
        event_service_config.is_some(),
        component_integrities.is_some(),
    ));
    let state = BmcState {
        event_service: event_service_config.map(crate::EventServiceState::new),
        event_sequence: Arc::default(),
        service_root_state,
        oem_state,
        manager,
        system_state,
        chassis_state,
        update_service_state,
        account_service_state,
        session_service_state,
        injection: injection.clone(),
        availability: availability.clone(),
        callbacks: Some(callbacks.clone()),
        component_integrities,
    };
    let account_service_state = state.account_service_state.clone();
    let session_service_state = state.session_service_state.clone();
    // DPU BMCs always require the factory password to be changed first.
    let permit_factory_default_password = matches!(
        &machine_info,
        MachineInfo::Host(h) if !h.hw_type.bmc_requires_factory_password_change()
    );
    let router = router
        .add_routes(|router| {
            crate::redfish::computer_system::add_routes(router, service_root_config.vendor)
        })
        .add_routes(crate::ipmi::add_routes)
        .add_routes(|router| extend(router, &state))
        .with_state(state.clone())
        .merge(crate::injection::management_router(injection.clone()));
    let router = ([
        // Innermost, so `$expand` sees an already filtered and paged collection.
        Box::new(redfish::query_router::append),
        Box::new(redfish::expander_router::append),
        Box::new(move |router| {
            if redfish_auth {
                let authorizer = Authorizer::new(account_service_state, session_service_state);
                let authorizer = if permit_factory_default_password {
                    authorizer.permit_factory_default_password()
                } else {
                    authorizer
                };
                auth_router::append(router, authorizer)
            } else {
                router
            }
        }),
        Box::new(move |router| {
            middleware_router::append(mat_host_id, router, injection, availability, callbacks)
        }),
        // Outermost: every bodiless error above, including auth and downtime, gets a Redfish envelope.
        Box::new(|router: Router| {
            router.layer(axum::middleware::from_fn(
                crate::http::redfish_error_envelope,
            ))
        }),
    ] as [Box<dyn FnOnce(axum::Router) -> axum::Router>; _])
        .into_iter()
        .fold(router, |router, f| f(router));
    let router = redfish::event_service::with_lifetime(router, state.event_service.as_ref());
    (router, state)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::{TestCallbacks, host_info};

    #[tokio::test]
    async fn omitted_event_service_has_no_discovery_or_routes() {
        // Limits cannot enable a service the profile omits; a caller can disable one it enables.
        let disabled = [
            (
                "profile omits the service, limits supplied",
                None,
                EventServiceOverride::Limits(EventServiceConfig::default()),
            ),
            (
                "caller disables an enabled profile",
                Some(EventServiceConfig::default()),
                EventServiceOverride::Disabled,
            ),
        ];
        for (scenario, profile, event_service) in disabled {
            let (router, state) = machine_router_inner(
                &host_info(crate::HardwareType::DellPowerEdgeR750),
                Arc::new(TestCallbacks::default()),
                "disabled-event-service".into(),
                false,
                Arc::new(InjectionStore::new()),
                MachineRouterOptions {
                    event_service,
                    ..Default::default()
                },
                profile,
                |router, _| router,
            );
            assert!(state.event_service.is_none(), "{scenario}");
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/redfish/v1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let root: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(root.get("EventService").is_none(), "{scenario}");
            for (scenario, (method, path), expected) in [
                (
                    "service absent",
                    ("GET", "/redfish/v1/EventService"),
                    StatusCode::NOT_FOUND,
                ),
                (
                    "stream absent",
                    ("GET", "/redfish/v1/EventService/SSE"),
                    StatusCode::NOT_FOUND,
                ),
                (
                    "HEAD absent",
                    ("HEAD", "/redfish/v1/EventService/SSE"),
                    StatusCode::NOT_FOUND,
                ),
                (
                    "subscriptions absent",
                    ("GET", "/redfish/v1/EventService/Subscriptions"),
                    StatusCode::NOT_FOUND,
                ),
                (
                    "stats absent",
                    ("GET", "/Mock/EventService/stats"),
                    StatusCode::NOT_FOUND,
                ),
                (
                    "untyped publication absent",
                    ("POST", "/Mock/EventService/events"),
                    StatusCode::NOT_FOUND,
                ),
                (
                    "untyped script absent",
                    ("POST", "/Mock/EventService/scripts"),
                    StatusCode::NOT_FOUND,
                ),
                (
                    "unknown member absent",
                    ("DELETE", "/redfish/v1/EventService/Subscriptions/garbage"),
                    StatusCode::NOT_FOUND,
                ),
            ] {
                let actual = {
                    let router = router.clone();
                    {
                        let response = router
                            .oneshot(
                                Request::builder()
                                    .method(method)
                                    .uri(path)
                                    .body(Body::empty())
                                    .unwrap(),
                            )
                            .await
                            .unwrap();
                        response.status()
                    }
                };
                assert_eq!(actual, expected, "{scenario}");
            }
        }
    }

    #[tokio::test]
    async fn factory_default_password_outside_account_service_follows_the_profile() {
        use axum_extra::headers::{Authorization, HeaderMapExt};

        use crate::machine_info::DpuSettings;
        use crate::test_support::TEST_MAC_POOL;
        use crate::{DpuMachineInfo, HardwareType};

        async fn get(router: &Router, path: &str, username: &str, password: &str) -> StatusCode {
            let mut request = Request::builder().uri(path).body(Body::empty()).unwrap();
            request
                .headers_mut()
                .typed_insert(Authorization::basic(username, password));
            router.clone().oneshot(request).await.unwrap().status()
        }

        let dpu = {
            let mut pool = TEST_MAC_POOL.lock().unwrap();
            MachineInfo::Dpu(DpuMachineInfo::new(
                HardwareType::DellPowerEdgeR750,
                &mut pool,
                DpuSettings::default(),
            ))
        };
        // HPE iLO and the power shelves serve every resource with factory
        // credentials; the other profiles refuse everything outside
        // AccountService until the password has been changed.
        let profiles = [
            (
                host_info(HardwareType::HpeProliantDl380aGen11),
                StatusCode::OK,
            ),
            (host_info(HardwareType::LiteOnPowerShelf), StatusCode::OK),
            (host_info(HardwareType::DeltaPowerShelf), StatusCode::OK),
            (
                host_info(HardwareType::DellPowerEdgeR750),
                StatusCode::FORBIDDEN,
            ),
            (
                host_info(HardwareType::LenovoGB300Nvl),
                StatusCode::FORBIDDEN,
            ),
            (
                host_info(HardwareType::SupermicroGb300Nvl),
                StatusCode::FORBIDDEN,
            ),
            (
                host_info(HardwareType::NvidiaDgxGb300),
                StatusCode::FORBIDDEN,
            ),
            (dpu, StatusCode::FORBIDDEN),
        ];
        for (machine_info, expected) in profiles {
            let scenario = match &machine_info {
                MachineInfo::Host(host) => host.hw_type.to_string(),
                MachineInfo::Dpu(_) => "DPU".to_string(),
            };
            let (router, state) = machine_router_inner(
                &machine_info,
                Arc::new(TestCallbacks::default()),
                "factory-password".into(),
                true,
                Arc::new(InjectionStore::new()),
                MachineRouterOptions::default(),
                None,
                |router, _| router,
            );
            let (username, factory) = state
                .account_service_state
                .administrator_credentials()
                .expect("an administrator account");
            for path in ["/redfish/v1/Managers", "/redfish/v1/Chassis"] {
                assert_eq!(
                    get(&router, path, &username, &factory).await,
                    expected,
                    "{scenario}: {path} with factory credentials"
                );
            }
            assert_eq!(
                get(
                    &router,
                    "/redfish/v1/AccountService/Accounts",
                    &username,
                    &factory
                )
                .await,
                StatusCode::OK,
                "{scenario}: AccountService with factory credentials"
            );
            assert_eq!(
                get(&router, "/redfish/v1/Managers", &username, "wrong").await,
                StatusCode::UNAUTHORIZED,
                "{scenario}: wrong password"
            );

            state
                .account_service_state
                .change_factory_default_password("rotated");
            assert_eq!(
                get(&router, "/redfish/v1/Managers", &username, "rotated").await,
                StatusCode::OK,
                "{scenario}: rotated password"
            );
        }
    }

    #[tokio::test]
    async fn extension_routes_and_layers_are_served_behind_redfish_middleware() {
        use axum::extract::{MatchedPath, Path, Request as AxumRequest, State};
        use axum::middleware::Next;
        use axum::response::{IntoResponse, Response};
        use axum::routing::post;
        use axum_extra::headers::{Authorization, HeaderMapExt};

        const DEVICE: &str = "/redfish/v1/Systems/{system_id}/VirtualMedia/{device_id}";
        const SYSTEM: &str = "/redfish/v1/Systems/System.Embedded.1";

        // An extension action that reads the BMC state it is bound to.
        async fn action(
            State(state): State<BmcState<TestCallbacks>>,
            Path((system_id, device_id)): Path<(String, String)>,
        ) -> Response {
            let drive = state.virtual_media_drive(&system_id, &device_id);
            axum::Json(serde_json::json!({"Drive": drive.is_some()})).into_response()
        }

        // A layer that adds an Oem property to the profile's drive resource.
        async fn decorate(path: MatchedPath, request: AxumRequest, next: Next) -> Response {
            let decorate = path.as_str() == DEVICE && request.method() == axum::http::Method::GET;
            let response = next.run(request).await;
            if !decorate {
                return response;
            }
            let (parts, body) = response.into_parts();
            let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            json["Oem"] = serde_json::json!({"Test": {"Decorated": true}});
            (parts, axum::Json(json)).into_response()
        }

        let (router, state) = machine_router_with_extension(
            &host_info(crate::HardwareType::DellPowerEdgeR750),
            Arc::new(TestCallbacks::default()),
            "extension".into(),
            true,
            MachineRouterOptions {
                virtual_media_devices: Some(vec![VirtualMediaDeviceConfig {
                    id: "Cd".into(),
                    name: "Virtual CD".into(),
                    media_types: vec!["CD".into()],
                }]),
                ..Default::default()
            },
            |router, _| {
                router
                    .route(&format!("{DEVICE}/Actions/Oem/Test.Action"), post(action))
                    .layer(axum::middleware::from_fn(decorate))
            },
        );
        let (username, _) = state
            .account_service_state
            .administrator_credentials()
            .expect("an administrator account");
        state
            .account_service_state
            .change_factory_default_password("rotated");
        let send = |method: &str, path: String, password: Option<&str>| {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            if let Some(password) = password {
                request
                    .headers_mut()
                    .typed_insert(Authorization::basic(&username, password));
            }
            router.clone().oneshot(request)
        };
        let action_path = format!("{SYSTEM}/VirtualMedia/Cd/Actions/Oem/Test.Action");

        let response = send("POST", action_path.clone(), None).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "extension routes require Redfish authentication"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            body.get("error").is_some(),
            "extension errors get the Redfish error envelope: {body}"
        );

        let response = send("POST", action_path, Some("rotated")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"Drive": true})
        );

        let response = send("GET", format!("{SYSTEM}/VirtualMedia/Cd"), Some("rotated"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["Oem"]["Test"]["Decorated"], true);
        assert_eq!(
            body["Id"], "Cd",
            "the profile's resource is decorated, not replaced"
        );
    }
}
