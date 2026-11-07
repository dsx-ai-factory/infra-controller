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

use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;

use super::schema::service_root::{
    Links, ProtocolFeaturesSupported, ServiceRoot, ServiceRootRequired,
};
use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::redfish::ModelExt;
use crate::{Callbacks, redfish};

/// Identity and navigation properties published by the service root.
#[derive(Clone, Copy)]
pub(crate) struct Config {
    pub(crate) vendor: redfish::oem::BmcVendor,
    pub(crate) product: Option<&'static str>,
    pub(crate) redfish_version: &'static str,
    /// Include the Systems navigation property independently of endpoint support.
    pub(crate) advertises_computer_systems: bool,
}

pub(crate) fn resource<'a>() -> redfish::Resource<'a> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1"),
        odata_type: Cow::Borrowed(redfish::schema::service_root::ServiceRoot::ODATA_TYPE),
        id: Cow::Borrowed("RootService"),
        name: Cow::Borrowed("Root Service"),
    }
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(&resource().odata_id, get(get_service_root::<C>))
}

fn builder(resource: &redfish::Resource) -> ServiceRoot {
    ServiceRoot::new(ServiceRootRequired {
        odata_id: resource.odata_id.to_string(),
        id: resource.id.to_string(),
        name: resource.name.to_string(),
        links: Links::new("/redfish/v1/SessionService/Sessions"),
    })
}

/// Configured ServiceRoot resource shared by the BMC handlers.
pub(crate) struct ServiceRootState {
    pub(crate) vendor: redfish::oem::BmcVendor,
    response: serde_json::Value,
}

impl ServiceRootState {
    pub(crate) fn from_config(
        config: Config,
        event_service: bool,
        component_integrity: bool,
    ) -> Self {
        let builder = builder(&resource())
            .redfish_version(config.redfish_version)
            .protocol_features_supported(ProtocolFeaturesSupported::new().filter_query(true))
            .maybe_with(
                |root, vendor: &str| root.vendor(vendor),
                &config.vendor.service_root_value(),
            )
            .maybe_with(|root, product: &str| root.product(product), &config.product)
            .account_service(redfish::account_service::resource().odata_id.as_ref())
            .session_service(
                redfish::session_service::service_resource()
                    .odata_id
                    .as_ref(),
            )
            .chassis(redfish::chassis::collection().odata_id.as_ref());
        let builder = if config.advertises_computer_systems {
            builder.systems(redfish::computer_system::collection().odata_id.as_ref())
        } else {
            builder
        };
        let builder = if event_service {
            builder.event_service(redfish::event_service::resource().odata_id.as_ref())
        } else {
            builder
        };
        // Attestation clients read this link to decide whether the BMC has
        // anything to attest, so only a configured collection is advertised.
        let builder = if component_integrity {
            builder
                .component_integrity(redfish::component_integrity::collection().odata_id.as_ref())
        } else {
            builder
        };
        let response = builder
            .managers(redfish::manager::collection().odata_id.as_ref())
            .update_service(redfish::update_service::resource().odata_id.as_ref())
            .telemetry_service(redfish::telemetry_service::resource().odata_id.as_ref())
            .into_json();

        Self {
            vendor: config.vendor,
            response,
        }
    }
}

async fn get_service_root<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    state.service_root_state.response.clone().into_ok_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::test_support::{TestBmcConfig, create_test_bmc, host_info};
    use crate::{HardwareType, MachineInfo, MachineRouterOptions};

    #[tokio::test]
    async fn profiles_publish_service_root_identity_and_systems_support() {
        for (hardware, vendor, product, version, advertises, serves) in [
            (
                HardwareType::DellPowerEdgeR750,
                Some("Dell"),
                None,
                "1.18.0",
                true,
                true,
            ),
            (
                HardwareType::DellPowerEdgeR760Bf4,
                Some("Dell"),
                Some("Integrated Dell Remote Access Controller"),
                "1.18.0",
                true,
                true,
            ),
            (
                HardwareType::WiwynnGB200Nvl,
                Some("WIWYNN"),
                Some("GB200 NVL"),
                "1.17.0",
                true,
                true,
            ),
            (
                HardwareType::LenovoGB300Nvl,
                Some("AMI"),
                Some("AMI Redfish Server"),
                "1.21.1",
                true,
                true,
            ),
            (
                HardwareType::NvidiaDgxGb300,
                Some("NVIDIA"),
                Some("GB BMC"),
                "1.17.0",
                true,
                true,
            ),
            (
                HardwareType::SupermicroGb300Nvl,
                Some("Supermicro"),
                Some("GB NVL"),
                "1.17.0",
                true,
                true,
            ),
            (
                HardwareType::NvidiaDgxVr,
                Some("NVIDIA"),
                Some("VR NVL72"),
                "1.17.0",
                true,
                true,
            ),
            (
                HardwareType::LiteOnPowerShelf,
                None,
                None,
                "1.9.0",
                false,
                true,
            ),
            (
                HardwareType::DeltaPowerShelf,
                None,
                None,
                "1.9.0",
                false,
                false,
            ),
            (
                HardwareType::NvidiaSwitchNd5200Ld,
                Some("NVIDIA"),
                Some("P3809"),
                "1.17.0",
                true,
                true,
            ),
            (
                HardwareType::NvidiaSwitchN5700Ld,
                Some("NVIDIA"),
                Some("P3809"),
                "1.17.0",
                true,
                true,
            ),
            (
                HardwareType::NvidiaDgxH100,
                Some("AMI"),
                Some("AMI Redfish Server"),
                "1.11.0",
                true,
                true,
            ),
            (
                HardwareType::GenericAmi,
                Some("AMI"),
                Some("AMI Redfish Server"),
                "1.17.0",
                true,
                true,
            ),
            (
                HardwareType::HpeProliantDl380aGen11,
                Some("HPE"),
                Some("ProLiant DL380a Gen11"),
                "1.13.0",
                true,
                true,
            ),
            (
                HardwareType::GenericSupermicro,
                Some("Supermicro"),
                Some("Super Server"),
                "1.17.0",
                true,
                true,
            ),
        ] {
            check_root(
                host_info(hardware),
                vendor,
                product,
                version,
                advertises,
                serves,
            )
            .await;
        }

        for (hardware, product) in [
            (HardwareType::WiwynnGB200Nvl, "BlueField-3 DPU"),
            (HardwareType::DellPowerEdgeR760Bf4, "BlueField-4"),
        ] {
            let MachineInfo::Host(mut host) = host_info(hardware) else {
                panic!("expected host fixture");
            };
            check_root(
                MachineInfo::Dpu(host.dpus.remove(0)),
                Some("Nvidia"),
                Some(product),
                "1.17.0",
                true,
                true,
            )
            .await;
        }
    }

    async fn check_root(
        machine: MachineInfo,
        vendor: Option<&str>,
        product: Option<&str>,
        version: &str,
        advertises: bool,
        serves: bool,
    ) {
        let (router, state) = create_test_bmc(
            &machine,
            TestBmcConfig::default(),
            "test-host-id".into(),
            false,
            MachineRouterOptions::default(),
        );
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
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let root: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            root.get("Vendor").and_then(serde_json::Value::as_str),
            vendor,
            "{machine:?}"
        );
        assert_eq!(
            root.get("Product").and_then(serde_json::Value::as_str),
            product,
            "{machine:?}"
        );
        assert_eq!(root["RedfishVersion"], version, "{machine:?}");
        assert_eq!(root.get("Systems").is_some(), advertises, "{machine:?}");
        assert_eq!(
            root.get("EventService").is_some(),
            state.event_service.is_some(),
            "{machine:?}"
        );
        assert_eq!(
            root.get("ComponentIntegrity").is_some(),
            state.component_integrities.is_some(),
            "{machine:?}"
        );
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1/Systems")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if serves {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
            "{machine:?}"
        );
        if !advertises && serves {
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let collection: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(collection["Members@odata.count"], 1);
            assert_eq!(
                collection["Members"][0]["@odata.id"],
                "/redfish/v1/Systems/system"
            );
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/redfish/v1/Systems/system")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let system: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(system["Id"], "system");
        }
    }
}
