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
use tokio::sync::oneshot;

use crate::auth_router::Authorizer;
use crate::bmc_state::BmcState;
use crate::injection::InjectionStore;
use crate::redfish::manager::ManagerState;
use crate::{
    Callbacks, EventServiceConfig, HardwareType, MachineInfo, SystemPowerControl,
    VirtualMediaDeviceConfig, auth_router, middleware_router, redfish,
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
}

#[derive(Debug)]
pub enum BmcCommand {
    SetSystemPower {
        request: SystemPowerControl,
        reply: Option<oneshot::Sender<SetSystemPowerResult>>,
    },
    StateRefreshIndication,
}

pub type SetSystemPowerResult = Result<(), SetSystemPowerError>;

#[derive(Debug, thiserror::Error)]
pub enum SetSystemPowerError {
    #[error("mock BMC reported bad request when setting system power: {0}")]
    BadRequest(String),
    #[error("mock BMC failed to send power command: {0}")]
    CommandSendError(String),
}

trait AddRoutes {
    fn add_routes(self, f: impl FnOnce(Self) -> Self) -> Self
    where
        Self: Sized;
}

impl AddRoutes for Router<BmcState> {
    fn add_routes(self, f: impl FnOnce(Self) -> Self) -> Self {
        f(self)
    }
}

/// Return an axum::Router that mocks various redfish calls to match
/// the provided MachineInfo.
pub fn machine_router(
    machine_info: &MachineInfo,
    callbacks: Arc<dyn Callbacks>,
    mat_host_id: String,
    redfish_auth: bool,
    options: MachineRouterOptions,
) -> (Router, BmcState) {
    machine_router_inner(
        machine_info,
        callbacks,
        mat_host_id,
        redfish_auth,
        Arc::new(InjectionStore::new()),
        options,
        machine_info.event_service_config(),
    )
}

/// Return a machine router backed by a caller-provided injection store.
pub fn machine_router_with_injection_store(
    machine_info: &MachineInfo,
    callbacks: Arc<dyn Callbacks>,
    mat_host_id: String,
    redfish_auth: bool,
    injection: Arc<InjectionStore>,
    options: MachineRouterOptions,
) -> (Router, BmcState) {
    machine_router_inner(
        machine_info,
        callbacks,
        mat_host_id,
        redfish_auth,
        injection,
        options,
        machine_info.event_service_config(),
    )
}

fn machine_router_inner(
    machine_info: &MachineInfo,
    callbacks: Arc<dyn Callbacks>,
    mat_host_id: String,
    redfish_auth: bool,
    injection: Arc<InjectionStore>,
    options: MachineRouterOptions,
    profile_event_service: Option<EventServiceConfig>,
) -> (Router, BmcState) {
    let system_config = machine_info.system_config(callbacks.clone());
    let chassis_config = machine_info.chassis_config();
    let update_service_config = machine_info.update_service_config();
    let bmc_vendor = machine_info.bmc_vendor();
    let bmc_product = machine_info.bmc_product();
    let bmc_redfish_version = machine_info.bmc_redfish_version();
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
        .add_routes(crate::redfish::manager::add_routes)
        .add_routes(crate::redfish::update_service::add_routes)
        .add_routes(crate::redfish::task_service::add_routes)
        .add_routes(crate::redfish::telemetry_service::add_routes)
        .add_routes(crate::redfish::account_service::add_routes)
        .add_routes(crate::redfish::session_service::add_routes)
        .add_routes(|routes| crate::redfish::computer_system::add_routes(routes, bmc_vendor))
        .add_routes(crate::redfish::virtual_media::add_routes)
        .add_routes(crate::ipmi::add_routes);
    let router = match machine_info {
        MachineInfo::Dpu(_) => {
            router.add_routes(crate::redfish::oem::nvidia::bluefield::add_routes)
        }
        MachineInfo::Host(_) => router
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
    let state = BmcState {
        event_service: event_service_config.map(crate::EventServiceState::new),
        event_sequence: Arc::default(),
        bmc_vendor,
        bmc_product,
        bmc_redfish_version,
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
        exposes_computer_systems: machine_info.exposes_computer_systems(),
    };
    let account_service_state = state.account_service_state.clone();
    let session_service_state = state.session_service_state.clone();
    let permit_factory_default_password = matches!(
        &machine_info,
        MachineInfo::Host(h) if matches!(
            h.hw_type,
            HardwareType::LiteOnPowerShelf | HardwareType::DeltaPowerShelf
        )
    );
    let router = router
        .with_state(state.clone())
        .merge(crate::injection::management_router(injection.clone()));
    let router = ([
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
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::{NoopCallbacks, host_info};

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
                Arc::new(NoopCallbacks),
                "disabled-event-service".into(),
                false,
                Arc::new(InjectionStore::new()),
                MachineRouterOptions {
                    event_service,
                    ..Default::default()
                },
                profile,
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
            check_cases_async(
                [
                    Case {
                        scenario: "service absent",
                        input: ("GET", "/redfish/v1/EventService"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                    Case {
                        scenario: "stream absent",
                        input: ("GET", "/redfish/v1/EventService/SSE"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                    Case {
                        scenario: "HEAD absent",
                        input: ("HEAD", "/redfish/v1/EventService/SSE"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                    Case {
                        scenario: "subscriptions absent",
                        input: ("GET", "/redfish/v1/EventService/Subscriptions"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                    Case {
                        scenario: "stats absent",
                        input: ("GET", "/Mock/EventService/stats"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                    Case {
                        scenario: "untyped publication absent",
                        input: ("POST", "/Mock/EventService/events"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                    Case {
                        scenario: "untyped script absent",
                        input: ("POST", "/Mock/EventService/scripts"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                    Case {
                        scenario: "unknown member absent",
                        input: ("DELETE", "/redfish/v1/EventService/Subscriptions/garbage"),
                        expect: Yields(StatusCode::NOT_FOUND),
                    },
                ],
                |(method, path)| {
                    let router = router.clone();
                    async move {
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
                        Ok::<_, std::convert::Infallible>(response.status())
                    }
                },
            )
            .await;
        }
    }
}
