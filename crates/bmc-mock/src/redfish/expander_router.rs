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
use std::str::FromStr;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use carbide_axum_utils::router::call_router_with_new_request;
use futures::future::join_all;
use itertools::Itertools;
use serde_json::Value;

// Add support of `$expand=.($levels=N)` per the redfish spec
//
// https://www.dmtf.org/sites/default/files/standards/documents/DSP0268_2024.2.pdf
pub(crate) fn append(router: Router) -> Router {
    Router::new()
        .route("/{*all}", get(process).fallback(fallback))
        .with_state(Expander { inner: router })
}

async fn fallback(State(mut state): State<Expander>, request: Request<Body>) -> Response {
    state.call_inner_router(request).await
}

async fn process(State(mut state): State<Expander>, request: Request<Body>) -> Response {
    let expand_level = expansion_level(&request);
    let response = state.call_inner_router(request).await;

    // Parse the ?$expand=.$($levels=1) param
    let Some(expand_level) = expand_level else {
        return response;
    };

    if expand_level == 0 {
        return response;
    }

    if !response.status().is_success() {
        // Don't rewrite failed responses
        return response;
    }

    let (parts, response_bytes) = match json_bytes(response).await {
        Ok(buffered) => buffered,
        // Expansion is defined for JSON collections, never streaming bodies.
        Err(BufferError::NotJson(response)) => return response,
        Err(BufferError::Read(e)) => {
            // Pretty sure this would only fail if body > usize::MAX.
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not read inner response body, {e}"),
            )
                .into_response();
        }
    };

    let mut json = match serde_json::from_slice::<HashMap<String, Value>>(response_bytes.as_ref()) {
        Ok(j) => j,
        Err(_) => {
            // Don't log an error if we couldn't decode the JSON, it's probably just not a redfish request.
            return (parts, response_bytes).into_response();
        }
    };
    let Some(Value::Array(members)) = json.remove("Members").or(json.remove("members")) else {
        // This error is slightly more suspicious, log it
        tracing::warn!("inner response JSON did not contain Members, returning it as-is");
        return (parts, response_bytes).into_response();
    };

    // Members look like: { "@odata.id": "/redfish/v1/Systems/1" }
    // Get the @odata.id URI strings
    let member_uris = members
        .iter()
        .filter_map(|member| match member {
            Value::Object(object) => Some(object),
            _ => {
                tracing::warn!(
                    member = ?member,
                    "Invalid member JSON, expected Object",
                );
                None
            }
        })
        .filter_map(|object| match object.get("@odata.id") {
            Some(Value::String(id)) => Some(id.to_owned()),
            _ => {
                tracing::warn!(
                    object = ?object,
                    "Invalid member JSON, expected @odata.id string",
                );
                None
            }
        })
        .collect::<Vec<_>>();

    if member_uris.len() != members.len() {
        // If we had to skip any of them, don't proceed (we already logged the error above), return the original JSON unexpanded
        return (parts, response_bytes).into_response();
    }

    // Transform them to the full result of fetching each URI from the inner router
    let expanded_members: Result<Vec<Value>, MemberRequestError> =
        join_all(member_uris.into_iter().map(|uri| {
            let mut state = state.clone();
            async move {
                let response = if expand_level > 1 {
                    // Recurse into one more level
                    let req = Request::builder()
                        .method(Method::GET)
                        .uri(format!(
                            "{}?$expand=.($levels={})",
                            uri.clone(),
                            expand_level - 1
                        ))
                        .body(Body::empty())
                        .unwrap();
                    process(State(state), req).await
                } else {
                    let req = Request::builder()
                        .method(Method::GET)
                        .uri(uri.clone())
                        .body(Body::empty())
                        .unwrap();
                    state.call_inner_router(req).await
                };
                let (parts, response_bytes) = match json_bytes(response).await {
                    Ok(buffered) => buffered,
                    Err(BufferError::NotJson(response)) => {
                        return Err(MemberRequestError::NotJson(uri, response.status()));
                    }
                    Err(BufferError::Read(e)) => return Err(MemberRequestError::Axum(uri, e)),
                };

                // Don't bother deserializing if it's unsuccessful
                if !parts.status.is_success() {
                    return Err(MemberRequestError::UnsuccessfulResponse(
                        uri,
                        parts,
                        String::from_utf8_lossy(response_bytes.to_vec().as_slice()).to_string(),
                    ));
                }

                serde_json::from_slice(response_bytes.as_ref()).map_err(|_| {
                    MemberRequestError::MalformedResponse(
                        uri,
                        String::from_utf8_lossy(response_bytes.to_vec().as_slice()).to_string(),
                    )
                })
            }
        }))
        .await
        .into_iter()
        .try_collect();

    let expanded_members = match expanded_members {
        Ok(v) => v,
        Err(error) => {
            // If any sub-request failed, return the original response
            tracing::warn!(%error, "Failed to expand Members object failed");
            return (parts, response_bytes).into_response();
        }
    };

    json.insert("Members".to_string(), Value::Array(expanded_members));

    // Keep the inner response's headers, including its JSON content type.
    let mut parts = parts;
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    (parts, serde_json::to_vec(&json).expect("serde error")).into_response()
}

fn expansion_level<T>(request: &Request<T>) -> Option<u8> {
    if let Some(query) = request.uri().query() {
        let params: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        params
            .get("$expand")
            .and_then(|val| {
                if val.starts_with(".($levels=") || val.starts_with("*($levels=") {
                    val.split("=").last().map(|s| s.replace(")", ""))
                } else if val == "*" || val == "." {
                    Some("1".into())
                } else {
                    None
                }
            })
            .and_then(|s| u8::from_str(&s).ok())
    } else {
        None
    }
}

#[derive(Debug, Clone)]
struct Expander {
    inner: Router,
}

enum BufferError {
    /// The response does not declare JSON; it is returned untouched.
    NotJson(Response),
    Read(axum::Error),
}

/// Buffer a JSON response body. Streaming and other non-JSON bodies are never
/// read to their end, which for an open event stream would be never.
async fn json_bytes(
    response: Response,
) -> Result<(axum::http::response::Parts, bytes::Bytes), BufferError> {
    if !carbide_axum_utils::is_json_response(&response) {
        return Err(BufferError::NotJson(response));
    }
    let (parts, body) = response.into_parts();
    axum::body::to_bytes(body, usize::MAX)
        .await
        .map(|bytes| (parts, bytes))
        .map_err(BufferError::Read)
}

#[derive(thiserror::Error, Debug)]
enum MemberRequestError {
    #[error("inner request to URI {0} returned failure: {1:?}, body: {2}")]
    UnsuccessfulResponse(String, axum::http::response::Parts, String),
    #[error("inner request to URI {0} returned a non-JSON {1} response")]
    NotJson(String, StatusCode),
    #[error("inner request to URI {0} returned a malformed response: {1}")]
    MalformedResponse(String, String),
    #[error("error reading bytes from inner request to {0}")]
    Axum(String, axum::Error),
}

impl Expander {
    /// See docs in `call_router_with_new_request`
    async fn call_inner_router(&mut self, request: Request<Body>) -> axum::response::Response {
        call_router_with_new_request(&mut self.inner, request).await
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn expansion_passes_through_an_open_stream() {
        use futures::{StreamExt, stream};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let router = super::append(axum::Router::new().route(
            "/stream",
            axum::routing::get(|| async {
                let body = axum::body::Body::from_stream(
                    stream::once(async { Ok::<_, std::io::Error>("data: {}\n\n") })
                        .chain(stream::pending()),
                );
                ([("content-type", "text/event-stream")], body)
            }),
        ));
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            router.oneshot(
                axum::http::Request::builder()
                    .uri("/stream?$expand=.($levels=1)")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("expansion must not wait for stream EOF")
        .unwrap();
        let mut body = response.into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "data: {}\n\n"
        );
    }

    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use serde_json::Value;
    use tower::Service;

    use crate::test_support::TEST_MAC_POOL;
    use crate::*;

    #[derive(Debug)]
    struct TestCallbacks {}

    impl Callbacks for TestCallbacks {
        fn get_power_state(&self) -> MockPowerState {
            MockPowerState::On
        }
        fn send_power_command(&self, _: SystemPowerControl) -> Result<(), SetSystemPowerError> {
            Ok(())
        }
        fn state_refresh_indication(&self) {}
    }

    fn test_host_mock() -> Router {
        let callbacks = Arc::new(TestCallbacks {});
        let mut mac_pool = TEST_MAC_POOL.lock().unwrap();
        let hw_type = HardwareType::DellPowerEdgeR750;
        let ranges_config = mac_pool.allocate_range_config().unwrap();
        crate::machine_router(
            &MachineInfo::Host(HostMachineInfo::new(
                hw_type,
                vec![DpuMachineInfo::new(
                    hw_type,
                    &mut mac_pool,
                    DpuSettings::default(),
                )],
                &mut mac_pool,
                ranges_config,
            )),
            callbacks,
            String::default(),
            false,
            MachineRouterOptions::default(),
        )
        .0
    }

    #[tokio::test]
    async fn test_expand() {
        let bmc_mock = test_host_mock();
        let mut subject = redfish::expander_router::append(bmc_mock.clone());

        let response_body = subject
            .call(
                Request::builder()
                    .uri("/redfish/v1/Chassis/System.Embedded.1/NetworkAdapters?$expand=.($levels=1)")
                    .method(Method::GET)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body();

        let Ok(Some(Value::Object(response_object))) = serde_json::from_slice(
            axum::body::to_bytes(response_body, usize::MAX)
                .await
                .unwrap()
                .as_ref(),
        ) else {
            panic!("Could not decode NetworkAdapters")
        };

        let Some(Value::Array(network_adapters)) = response_object.get("Members") else {
            panic!("No Members array in {response_object:?}")
        };

        // Make sure each network adapter deserializes into what we expect, and that it matches what is held in the upstream router.
        for network_adapter in network_adapters {
            // Check that network_adapter is full object.
            assert!(network_adapter.get("Manufacturer").is_some());
            let Some(odata_id) = network_adapter
                .get("@odata.id")
                .and_then(serde_json::Value::as_str)
            else {
                panic!("Network adapter must contain @odata.id");
            };
            let upstream_network_adapter_body = subject
                .call(
                    Request::builder()
                        .uri(odata_id)
                        .method(Method::GET)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .expect("Could not call tar_router")
                .into_body();

            let Ok(upstream_network_adapter) = serde_json::from_slice::<Value>(
                axum::body::to_bytes(upstream_network_adapter_body, usize::MAX)
                    .await
                    .unwrap()
                    .as_ref(),
            ) else {
                panic!("Could not deserialize tar_router response")
            };

            assert_eq!(network_adapter, &upstream_network_adapter)
        }
    }
}
