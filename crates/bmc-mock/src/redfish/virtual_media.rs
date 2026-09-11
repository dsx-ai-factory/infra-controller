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
use std::sync::Mutex;

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::{http, redfish};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceConfig {
    pub id: Cow<'static, str>,
    pub name: Cow<'static, str>,
    pub media_types: Vec<Cow<'static, str>>,
}

#[derive(Default)]
struct Media {
    image: Option<String>,
    inserted: bool,
    write_protected: bool,
}

struct DeviceState {
    config: DeviceConfig,
    media: Mutex<Media>,
}

pub(crate) struct VirtualMediaState {
    devices: Vec<DeviceState>,
}

impl VirtualMediaState {
    pub(super) fn new(devices: Vec<DeviceConfig>) -> Self {
        Self {
            devices: devices
                .into_iter()
                .map(|config| DeviceState {
                    config,
                    media: Mutex::new(Media {
                        write_protected: true,
                        ..Default::default()
                    }),
                })
                .collect(),
        }
    }

    fn find_device(&self, device_id: &str) -> Option<&DeviceState> {
        self.devices
            .iter()
            .find(|device| device.config.id == device_id)
    }

    pub(crate) fn desired_state(&self) -> Vec<serde_json::Value> {
        self.devices.iter().map(DeviceState::state_json).collect()
    }
}

pub(super) fn collection(system_id: &str) -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Owned(format!(
            "{}/VirtualMedia",
            redfish::computer_system::resource(system_id).odata_id
        )),
        odata_type: Cow::Borrowed("#VirtualMediaCollection.VirtualMediaCollection"),
        name: Cow::Borrowed("Virtual Media Collection"),
    }
}

fn resource<'a>(system_id: &str, device_id: &'a str) -> redfish::Resource<'a> {
    redfish::Resource {
        odata_id: Cow::Owned(format!("{}/{device_id}", collection(system_id).odata_id)),
        odata_type: Cow::Borrowed("#VirtualMedia.v1_3_2.VirtualMedia"),
        id: Cow::Borrowed(device_id),
        name: Cow::Borrowed("Virtual Media"),
    }
}

fn insert_media_target(system_id: &str, device_id: &str) -> String {
    format!(
        "{}/Actions/VirtualMedia.InsertMedia",
        resource(system_id, device_id).odata_id
    )
}

fn eject_media_target(system_id: &str, device_id: &str) -> String {
    format!(
        "{}/Actions/VirtualMedia.EjectMedia",
        resource(system_id, device_id).odata_id
    )
}

pub(crate) fn add_routes(router: Router<BmcState>) -> Router<BmcState> {
    const SYSTEM_ID: &str = "{system_id}";
    const DEVICE_ID: &str = "{device_id}";
    router
        .route(&collection(SYSTEM_ID).odata_id, get(get_collection))
        .route(&resource(SYSTEM_ID, DEVICE_ID).odata_id, get(get_device))
        .route(
            &insert_media_target(SYSTEM_ID, DEVICE_ID),
            post(insert_media),
        )
        .route(&eject_media_target(SYSTEM_ID, DEVICE_ID), post(eject_media))
}

async fn get_collection(State(state): State<BmcState>, Path(system_id): Path<String>) -> Response {
    let Some(virtual_media) = state
        .system_state
        .find(&system_id)
        .and_then(|system| system.virtual_media())
    else {
        return http::not_found();
    };
    let members = virtual_media
        .devices
        .iter()
        .map(|device| resource(&system_id, &device.config.id).entity_ref())
        .collect::<Vec<_>>();
    collection(&system_id)
        .with_members(&members)
        .into_ok_response()
}

async fn get_device(
    State(state): State<BmcState>,
    Path((system_id, device_id)): Path<(String, String)>,
) -> Response {
    let Some(device) = state
        .system_state
        .find(&system_id)
        .and_then(|system| system.virtual_media())
        .and_then(|virtual_media| virtual_media.find_device(&device_id))
    else {
        return http::not_found();
    };
    device.to_json(&system_id).into_ok_response()
}

async fn insert_media(
    State(state): State<BmcState>,
    Path((system_id, device_id)): Path<(String, String)>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    let Some(device) = state
        .system_state
        .find(&system_id)
        .and_then(|system| system.virtual_media())
        .and_then(|virtual_media| virtual_media.find_device(&device_id))
    else {
        return http::not_found();
    };
    let Some(image) = request.get("Image").and_then(serde_json::Value::as_str) else {
        return http::bad_request("Image must be a string");
    };
    if image.is_empty() {
        return http::bad_request("Image must not be empty");
    }
    match request.get("Inserted") {
        Some(serde_json::Value::Bool(false)) => {
            return http::bad_request("Inserted must not be false for InsertMedia");
        }
        Some(serde_json::Value::Bool(true)) | None => {}
        Some(_) => return http::bad_request("Inserted must be a boolean"),
    }
    let write_protected = match request.get("WriteProtected") {
        Some(serde_json::Value::Bool(value)) => *value,
        None => true,
        Some(_) => return http::bad_request("WriteProtected must be a boolean"),
    };
    *device.media.lock().expect("mutex poisoned") = Media {
        image: Some(image.to_string()),
        inserted: true,
        write_protected,
    };
    http::ok_no_content()
}

async fn eject_media(
    State(state): State<BmcState>,
    Path((system_id, device_id)): Path<(String, String)>,
) -> Response {
    let Some(device) = state
        .system_state
        .find(&system_id)
        .and_then(|system| system.virtual_media())
        .and_then(|virtual_media| virtual_media.find_device(&device_id))
    else {
        return http::not_found();
    };
    *device.media.lock().expect("mutex poisoned") = Media {
        write_protected: true,
        ..Default::default()
    };
    http::ok_no_content()
}

impl DeviceState {
    fn state_json(&self) -> serde_json::Value {
        let media = self.media.lock().expect("mutex poisoned");
        json!({
            "Id": self.config.id,
            "Image": media.image,
            "Inserted": media.inserted,
            "WriteProtected": media.write_protected,
        })
    }

    fn to_json(&self, system_id: &str) -> serde_json::Value {
        let resource = resource(system_id, &self.config.id);
        resource.json_patch().patch(self.state_json()).patch(json!({
            "Name": self.config.name,
            "MediaTypes": self.config.media_types,
            "ConnectedVia": "URI",
            "Actions": {
                "#VirtualMedia.InsertMedia": {
                    "target": insert_media_target(system_id, &self.config.id),
                },
                "#VirtualMedia.EjectMedia": {
                    "target": eject_media_target(system_id, &self.config.id),
                },
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::host_info;
    use crate::{
        Callbacks, HardwareType, MachineRouterOptions, MockPowerState, SetSystemPowerError,
        SystemPowerControl, machine_router,
    };

    #[derive(Debug, Default)]
    struct RecordingCallbacks {
        refresh_count: AtomicUsize,
    }

    impl Callbacks for RecordingCallbacks {
        fn get_power_state(&self) -> MockPowerState {
            MockPowerState::Off
        }

        fn send_power_command(
            &self,
            _reset_type: SystemPowerControl,
        ) -> Result<(), SetSystemPowerError> {
            Ok(())
        }

        fn state_refresh_indication(&self) {
            self.refresh_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn test_router() -> (Router, Arc<RecordingCallbacks>) {
        test_router_for(HardwareType::DellPowerEdgeR750)
    }

    fn test_router_for(hardware_type: HardwareType) -> (Router, Arc<RecordingCallbacks>) {
        let callbacks = Arc::new(RecordingCallbacks::default());
        let router = machine_router(
            &host_info(hardware_type),
            callbacks.clone(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions {
                event_service: crate::EventServiceOverride::Profile,
                bmc_reset_duration: None,
                virtual_media_devices: Some(vec![
                    DeviceConfig {
                        id: "Cd".into(),
                        name: "Operating System Virtual CD".into(),
                        media_types: vec!["CD".into(), "DVD".into()],
                    },
                    DeviceConfig {
                        id: "ConfigCd".into(),
                        name: "Configuration Virtual CD".into(),
                        media_types: vec!["CD".into(), "DVD".into()],
                    },
                ]),
            },
        )
        .0;
        (router, callbacks)
    }

    #[tokio::test]
    async fn attaches_virtual_media_to_the_controlled_system_not_the_first_member() {
        let (router, _) = test_router_for(HardwareType::NvidiaDgxGb300);
        let hgx = "/redfish/v1/Systems/HGX_Baseboard_0";
        let host = "/redfish/v1/Systems/System_0";

        let (status, body) = request(&router, Method::GET, hgx, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.unwrap().get("VirtualMedia").is_none());

        let (status, body) = request(&router, Method::GET, host, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.unwrap()["VirtualMedia"]["@odata.id"],
            format!("{host}/VirtualMedia")
        );
    }

    async fn request(
        router: &Router,
        method: Method,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, Option<serde_json::Value>) {
        let mut request = Request::builder().method(method).uri(uri);
        let body = if let Some(body) = body {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let response = router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = (!body.is_empty()).then(|| serde_json::from_slice(&body).unwrap());
        (status, body)
    }

    #[tokio::test]
    async fn exposes_and_controls_two_independent_virtual_media_devices() {
        let (router, callbacks) = test_router();
        let system = "/redfish/v1/Systems/System.Embedded.1";

        let (status, body) = request(&router, Method::GET, system, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.unwrap()["VirtualMedia"]["@odata.id"],
            format!("{system}/VirtualMedia")
        );

        let insert = |image: &str| {
            json!({
                "Image": image,
                "Inserted": true,
                "WriteProtected": true,
            })
        };
        let (status, _) = request(
            &router,
            Method::POST,
            &format!("{system}/VirtualMedia/Cd/Actions/VirtualMedia.InsertMedia"),
            Some(insert("http://127.0.0.1:8080/installer.iso")),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = request(
            &router,
            Method::POST,
            &format!("{system}/VirtualMedia/ConfigCd/Actions/VirtualMedia.InsertMedia"),
            Some(insert("http://127.0.0.1:8080/config.iso")),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, body) = request(
            &router,
            Method::GET,
            &format!("{system}/VirtualMedia/Cd"),
            None,
        )
        .await;
        let body = body.unwrap();
        assert_eq!(body["Inserted"], true);
        assert_eq!(body["Image"], "http://127.0.0.1:8080/installer.iso");

        let (status, _) = request(
            &router,
            Method::POST,
            &format!("{system}/VirtualMedia/ConfigCd/Actions/VirtualMedia.EjectMedia"),
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = request(
            &router,
            Method::GET,
            &format!("{system}/VirtualMedia/Cd"),
            None,
        )
        .await;
        assert_eq!(body.unwrap()["Inserted"], true);
        let (_, body) = request(
            &router,
            Method::GET,
            &format!("{system}/VirtualMedia/ConfigCd"),
            None,
        )
        .await;
        assert_eq!(body.unwrap()["Inserted"], false);

        assert_eq!(callbacks.refresh_count.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn applies_cd_boot_override_on_the_computer_system_resource() {
        let (router, callbacks) = test_router();
        let system = "/redfish/v1/Systems/System.Embedded.1";

        let (status, _) = request(
            &router,
            Method::PATCH,
            system,
            Some(json!({
                "Boot": {
                    "BootSourceOverrideTarget": "Cd",
                    "BootSourceOverrideMode": "UEFI",
                    "BootSourceOverrideEnabled": "Once",
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(callbacks.refresh_count.load(Ordering::Relaxed), 1);

        let (_, body) = request(&router, Method::GET, system, None).await;
        let body = body.unwrap();
        assert_eq!(body["Boot"]["BootSourceOverrideMode"], "UEFI");
        assert_eq!(body["Boot"]["BootSourceOverrideEnabled"], "Once");
        assert_eq!(body["Boot"]["BootSourceOverrideTarget"], "Cd");
    }

    #[tokio::test]
    async fn preserves_unrecognized_boot_override_values() {
        let (router, callbacks) = test_router();
        let system = "/redfish/v1/Systems/System.Embedded.1";

        let (status, _) = request(
            &router,
            Method::PATCH,
            system,
            Some(json!({
                "Boot": {
                    "BootSourceOverrideTarget": "VendorSpecific",
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(callbacks.refresh_count.load(Ordering::Relaxed), 1);

        let (_, body) = request(&router, Method::GET, system, None).await;
        assert_eq!(
            body.unwrap()["Boot"]["BootSourceOverrideTarget"],
            "VendorSpecific"
        );
    }
}
