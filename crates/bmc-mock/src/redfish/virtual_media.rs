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
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::{ActionError, Callbacks, http, redfish};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceConfig {
    pub id: Cow<'static, str>,
    pub name: Cow<'static, str>,
    pub media_types: Vec<Cow<'static, str>>,
}

/// Contents of a virtual-media drive, independent of its Redfish JSON representation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum VirtualMediaContents {
    /// Empty drive after ejecting its media.
    #[default]
    Empty,
    /// Inserted media and its write policy.
    Inserted {
        /// Local path or URI of the media image.
        image: String,
        /// Whether the backend must expose the media read-only.
        write_protected: bool,
    },
}

/// One serialized virtual-media operation awaiting backend application.
///
/// The operation retains its drive lock when transferred to a backend actor, even
/// if the requesting HTTP future is cancelled. Call [`Self::commit`] after applying
/// it successfully, before replying. Dropping it without committing preserves the
/// previous Redfish state and releases the lock.
#[derive(Debug)]
pub struct VirtualMediaUpdate {
    /// ComputerSystem containing the drive.
    pub system_id: String,
    /// Configured virtual-media device identifier.
    pub device_id: String,
    /// Requested drive contents.
    pub contents: VirtualMediaContents,
    device: Arc<DeviceState>,
    _operation: tokio::sync::OwnedMutexGuard<()>,
}

impl VirtualMediaUpdate {
    /// Publishes successfully applied contents to Redfish and releases the drive lock.
    pub fn commit(self) {
        *self.device.media.lock().expect("mutex poisoned") = self.contents;
    }
}

#[derive(Debug)]
struct DeviceState {
    config: DeviceConfig,
    media: Mutex<VirtualMediaContents>,
    operation: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) struct VirtualMediaState {
    devices: Vec<Arc<DeviceState>>,
}

impl VirtualMediaState {
    pub(super) fn new(devices: Vec<DeviceConfig>) -> Self {
        Self {
            devices: devices
                .into_iter()
                .map(|config| {
                    Arc::new(DeviceState {
                        config,
                        media: Mutex::new(VirtualMediaContents::Empty),
                        operation: Arc::new(tokio::sync::Mutex::new(())),
                    })
                })
                .collect(),
        }
    }

    fn find_device(&self, device_id: &str) -> Option<&Arc<DeviceState>> {
        self.devices
            .iter()
            .find(|device| device.config.id == device_id)
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

pub(crate) fn add_routes<C: Callbacks>(router: Router<BmcState<C>>) -> Router<BmcState<C>> {
    const SYSTEM_ID: &str = "{system_id}";
    const DEVICE_ID: &str = "{device_id}";
    router
        .route(&collection(SYSTEM_ID).odata_id, get(get_collection::<C>))
        .route(
            &resource(SYSTEM_ID, DEVICE_ID).odata_id,
            get(get_device::<C>),
        )
        .route(
            &insert_media_target(SYSTEM_ID, DEVICE_ID),
            post(insert_media::<C>),
        )
        .route(
            &eject_media_target(SYSTEM_ID, DEVICE_ID),
            post(eject_media::<C>),
        )
}

async fn get_collection<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(system_id): Path<String>,
) -> Response {
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

async fn get_device<C: Callbacks>(
    State(state): State<BmcState<C>>,
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

async fn insert_media<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
    device
        .update(
            &state,
            system_id,
            VirtualMediaContents::Inserted {
                image: image.to_string(),
                write_protected,
            },
        )
        .await
}

async fn eject_media<C: Callbacks>(
    State(state): State<BmcState<C>>,
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
    device
        .update(&state, system_id, VirtualMediaContents::Empty)
        .await
}

impl DeviceState {
    async fn update<C: Callbacks>(
        self: &Arc<Self>,
        state: &BmcState<C>,
        system_id: String,
        contents: VirtualMediaContents,
    ) -> Response {
        let Some(callbacks) = &state.callbacks else {
            return http::redfish_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "virtual media has no backend",
            );
        };
        let update = VirtualMediaUpdate {
            system_id,
            device_id: self.config.id.to_string(),
            contents,
            device: self.clone(),
            _operation: self.operation.clone().lock_owned().await,
        };
        match callbacks.set_virtual_media(update).await {
            Ok(()) => http::ok_no_content(),
            Err(ActionError::BadRequest(error)) => http::bad_request(&error.to_string()),
            Err(ActionError::Internal(error)) => {
                tracing::warn!(device_id = %self.config.id, error = ?error, "virtual media update failed");
                http::redfish_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "virtual media update failed",
                )
            }
        }
    }

    fn state_json(&self) -> serde_json::Value {
        let media = self.media.lock().expect("mutex poisoned");
        let (image, inserted, write_protected) = match &*media {
            VirtualMediaContents::Empty => (None, false, true),
            VirtualMediaContents::Inserted {
                image,
                write_protected,
            } => (Some(image), true, *write_protected),
        };
        json!({
            "Id": self.config.id,
            "Image": image,
            "Inserted": inserted,
            "WriteProtected": write_protected,
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
    use std::sync::atomic::Ordering;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use nv_redfish::schema::resource::PowerState;
    use tokio::sync::{mpsc, oneshot};
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::{TestBmcConfig, TestCallbacks, create_test_bmc, host_info};
    use crate::{HardwareType, MachineRouterOptions};

    struct ControlledMediaCallbacks {
        requests:
            mpsc::UnboundedSender<(VirtualMediaUpdate, oneshot::Sender<Result<(), ActionError>>)>,
    }

    impl Callbacks for ControlledMediaCallbacks {
        async fn computer_system_reset(
            &self,
            _: crate::ResourceResetType,
        ) -> Result<(), ActionError> {
            Ok(())
        }

        async fn set_virtual_media(&self, update: VirtualMediaUpdate) -> Result<(), ActionError> {
            let (reply, response) = oneshot::channel();
            self.requests.send((update, reply)).unwrap();
            response.await.unwrap()
        }

        fn state_refresh_indication(&self) {}
    }

    #[tokio::test]
    async fn serializes_media_updates_after_http_cancellation() {
        let (requests, mut operations) = mpsc::unbounded_channel();
        let (router, _state) = crate::machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(ControlledMediaCallbacks { requests }),
            "test-host".to_owned(),
            false,
            MachineRouterOptions {
                virtual_media_devices: Some(vec![DeviceConfig {
                    id: "Cd".into(),
                    name: "Virtual CD".into(),
                    media_types: vec!["CD".into()],
                }]),
                ..Default::default()
            },
        );
        let drive = "/redfish/v1/Systems/System.Embedded.1/VirtualMedia/Cd";
        let insert = format!("{drive}/Actions/VirtualMedia.InsertMedia");
        let eject = format!("{drive}/Actions/VirtualMedia.EjectMedia");
        let mut first = Box::pin(request(
            &router,
            Method::POST,
            &insert,
            Some(json!({"Image": "first.iso"})),
        ));
        let (first_update, first_reply) = tokio::select! {
            result = &mut first => panic!("request completed before backend reply: {result:?}"),
            operation = operations.recv() => operation.unwrap(),
        };
        drop(first);
        let (_, body) = request(&router, Method::GET, drive, None).await;
        assert_eq!(
            body.unwrap()["Inserted"],
            false,
            "pending contents are not published"
        );

        let mut second = Box::pin(request(&router, Method::POST, &eject, Some(json!({}))));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(second.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(
            operations.try_recv().is_err(),
            "cancelling HTTP must not release the drive lock"
        );
        first_update.commit();
        assert!(
            first_reply.send(Ok(())).is_err(),
            "the original caller is gone"
        );

        let (second_update, second_reply) = tokio::select! {
            result = &mut second => panic!("request completed before backend reply: {result:?}"),
            operation = operations.recv() => operation.unwrap(),
        };
        assert_eq!(second_update.contents, VirtualMediaContents::Empty);
        let (_, body) = request(&router, Method::GET, drive, None).await;
        assert_eq!(body.unwrap()["Image"], "first.iso");
        drop(second_update);
        second_reply
            .send(Err(ActionError::Internal(eyre::eyre!("backend failed"))))
            .unwrap();
        assert_eq!(second.await.0, StatusCode::INTERNAL_SERVER_ERROR);
        let (_, body) = request(&router, Method::GET, drive, None).await;
        assert_eq!(
            body.unwrap()["Image"],
            "first.iso",
            "failed eject retains committed contents"
        );
    }

    fn test_router() -> (Router, Arc<TestCallbacks>) {
        test_router_for(HardwareType::DellPowerEdgeR750)
    }

    fn test_router_for(hardware_type: HardwareType) -> (Router, Arc<TestCallbacks>) {
        let (router, state) = create_test_bmc(
            &host_info(hardware_type),
            TestBmcConfig {
                power_state: PowerState::Off,
            },
            "test-host-id".to_string(),
            false,
            MachineRouterOptions {
                event_service: crate::EventServiceOverride::Profile,
                bmc_reset_duration: None,
                firmware_upgrade_duration: None,
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
        );
        let callbacks = state.callbacks.as_ref().unwrap();
        (router, callbacks.clone())
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
