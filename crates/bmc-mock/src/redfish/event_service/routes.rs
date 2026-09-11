// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Redfish discovery, SSE establishment, and mock HTTP controls.

use std::borrow::Cow;
use std::sync::{Arc, Weak};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use futures::stream;
use serde_json::{Value, json};

use super::{EventServiceError, EventServiceState, ROOT, SSE, SUBSCRIPTIONS, StreamStep};
use crate::BmcState;
use crate::combined_server::OutputStallTimeout;
use crate::http::redfish_error;
use crate::json::{JsonExt, JsonPatch};
use crate::redfish::{Collection, Resource};

impl IntoResponse for EventServiceError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        redfish_error(status, &self.to_string())
    }
}

pub(crate) fn add_routes(router: Router<BmcState>) -> Router<BmcState> {
    router
        .route(&resource().odata_id, get(service))
        .route(
            SSE,
            // Axum would otherwise serve HEAD through the GET handler and register a subscriber.
            get(events).head(|| async { (StatusCode::METHOD_NOT_ALLOWED, [("allow", "GET")]) }),
        )
        .route(SUBSCRIPTIONS, get(subscriptions))
        .route(
            "/redfish/v1/EventService/Subscriptions/{id}",
            get(subscription).delete(delete_subscription),
        )
        .route("/Mock/EventService/events", post(publish))
        .route("/Mock/EventService/stats", get(stats))
        .route("/Mock/EventService/close", post(close))
        .route(
            "/Mock/EventService/scripts",
            post(script).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
}

fn enabled(state: &BmcState) -> Result<Arc<EventServiceState>, EventServiceError> {
    state
        .event_service
        .clone()
        .ok_or(EventServiceError::NotFound)
}

pub(crate) fn resource() -> Resource<'static> {
    Resource {
        odata_id: Cow::Borrowed(ROOT),
        odata_type: Cow::Borrowed("#EventService.v1_2_0.EventService"),
        id: Cow::Borrowed("EventService"),
        name: Cow::Borrowed("Event Service"),
    }
}

fn subscription_collection() -> Collection<'static> {
    Collection {
        odata_id: Cow::Borrowed(SUBSCRIPTIONS),
        odata_type: Cow::Borrowed("#EventDestinationCollection.EventDestinationCollection"),
        name: Cow::Borrowed("SSE Subscriptions"),
    }
}

async fn service(State(state): State<BmcState>) -> Result<Response, EventServiceError> {
    enabled(&state)?;
    Ok(Json(
        resource()
            .json_patch()
            .patch(json!({
                "ServiceEnabled": true, "ServerSentEventUri": SSE,
                "EventFormatTypes": ["Event", "MetricReport"]
            }))
            .patch(subscription_collection().nav_property("Subscriptions")),
    )
    .into_response())
}

// HTTP list delimiters inside quoted parameter values are literal characters.
fn split_quoted(value: &str, delimiter: char) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    let mut escaped = false;
    value.split(move |ch| {
        if escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if !quoted && ch == delimiter {
            return true;
        }
        false
    })
}

/// Media-range negotiation for the served `text/event-stream` representation,
/// which carries no media parameters: parameters other than `q` are ignored, a
/// more specific range wins, and equally specific ranges take the highest weight.
fn accepts_sse(headers: &HeaderMap) -> bool {
    if !headers.contains_key("accept") {
        return true;
    }
    let mut selected: Option<(u8, f32)> = None;
    for value in headers.get_all("accept") {
        let Ok(value) = value.to_str() else {
            return false;
        };
        for range in split_quoted(value, ',') {
            let mut parts = split_quoted(range, ';');
            let specificity = match parts
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "text/event-stream" => 2,
                "text/*" => 1,
                "*/*" => 0,
                _ => continue,
            };
            let quality = parts
                .filter_map(|parameter| parameter.split_once('='))
                .find(|(key, _)| key.trim().eq_ignore_ascii_case("q"))
                .map_or(1.0, |(_, weight)| {
                    weight
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .filter(|q| (0.0..=1.0).contains(q))
                        .unwrap_or(0.0)
                });
            selected = Some(match selected {
                Some((s, q)) if s > specificity => (s, q),
                Some((s, q)) if s == specificity => (s, q.max(quality)),
                _ => (specificity, quality),
            });
        }
    }
    selected.is_some_and(|(_, quality)| quality > 0.0)
}

async fn events(
    State(state): State<BmcState>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, EventServiceError> {
    let state = enabled(&state)?;
    if uri.query().is_some() {
        return Err(EventServiceError::Invalid(
            "SSE query options are unsupported".into(),
        ));
    }
    if !accepts_sse(&headers) {
        return Ok(StatusCode::NOT_ACCEPTABLE.into_response());
    }
    let last_id = match headers.get("last-event-id").map(|v| v.to_str()) {
        Some(Err(_)) => {
            return Err(EventServiceError::Invalid(
                "invalid Last-Event-ID header".into(),
            ));
        }
        Some(Ok(id)) => Some(id),
        None => None,
    };
    let subscriber = state.subscribe(last_id)?;
    let body = Body::from_stream(stream::unfold(Some(subscriber), |subscriber| async {
        let mut subscriber = subscriber?;
        match subscriber.next().await {
            Some(Ok(bytes)) => Some((Ok(bytes), Some(subscriber))),
            Some(Err(error)) => Some((Err(error), None)),
            None => None,
        }
    }));
    let mut response = (
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
        ],
        body,
    )
        .into_response();
    // CombinedServer honors this bound; a bare router (in-process tests, other
    // embedders) serves the body without one.
    response
        .extensions_mut()
        .insert(OutputStallTimeout(state.config.limits.output_stall_timeout));
    Ok(response)
}

fn destination_resource(id: u64) -> Resource<'static> {
    Resource {
        odata_id: Cow::Owned(format!("{SUBSCRIPTIONS}/{id}")),
        odata_type: Cow::Borrowed("#EventDestination.v1_6_0.EventDestination"),
        id: Cow::Owned(id.to_string()),
        name: Cow::Borrowed("SSE Subscription"),
    }
}

fn destination(id: u64, context: String) -> Value {
    destination_resource(id)
        .json_patch()
        .patch(json!({"Context": context, "Protocol": "Redfish", "SubscriptionType": "SSE"}))
}

async fn subscriptions(State(state): State<BmcState>) -> Result<Response, EventServiceError> {
    let members: Vec<_> = enabled(&state)?
        .subscription_ids()
        .into_iter()
        .map(|id| destination_resource(id).entity_ref())
        .collect();
    Ok(Json(subscription_collection().with_members(&members)).into_response())
}

fn subscription_id(id: &str) -> Result<u64, EventServiceError> {
    id.parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == id)
        .ok_or(EventServiceError::NotFound)
}

async fn subscription(
    State(state): State<BmcState>,
    Path(id): Path<String>,
) -> Result<Response, EventServiceError> {
    let state = enabled(&state)?;
    let id = subscription_id(&id)?;
    let context = state
        .subscription_context(id)
        .ok_or(EventServiceError::NotFound)?;
    Ok(Json(destination(id, context)).into_response())
}

async fn delete_subscription(
    State(state): State<BmcState>,
    Path(id): Path<String>,
) -> Result<Response, EventServiceError> {
    let state = enabled(&state)?;
    if !state.delete_subscription(subscription_id(&id)?) {
        return Err(EventServiceError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn publish(
    State(state): State<BmcState>,
    Json(payload): Json<Value>,
) -> Result<Response, EventServiceError> {
    let id = enabled(&state)?.publish(payload)?;
    Ok(Json(json!({"id": id})).into_response())
}

async fn stats(State(state): State<BmcState>) -> Result<Response, EventServiceError> {
    Ok(Json(enabled(&state)?.stats()).into_response())
}

async fn close(State(state): State<BmcState>) -> Result<Response, EventServiceError> {
    enabled(&state)?.close_subscribers();
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn script(
    State(state): State<BmcState>,
    Json(steps): Json<Vec<StreamStep>>,
) -> Result<Response, EventServiceError> {
    enabled(&state)?.queue_script(steps)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

struct RouterLease(Weak<EventServiceState>);
impl Drop for RouterLease {
    fn drop(&mut self) {
        if let Some(state) = self.0.upgrade() {
            state.close_on_drop();
        }
    }
}

/// Tie active streams to the router's lifetime. Only router clones own this
/// lease; response streams own the event state alone, so removing the final
/// router closes streams even when a test retains `BmcState` to inspect cleanup.
pub(crate) fn with_lifetime(router: Router, state: Option<&Arc<EventServiceState>>) -> Router {
    match state {
        Some(state) => router.layer(Extension(Arc::new(RouterLease(Arc::downgrade(state))))),
        None => router,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_header_honors_media_ranges_and_weights() {
        carbide_test_support::value_scenarios!(run = |accept: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("accept", accept.parse().unwrap());
            accepts_sse(&headers)
        };
            "compatible ranges" {
                "*/*" => true,
                "text/*;q=0.5" => true,
                "text/event-stream;charset=utf-8" => true,
                "text/event-stream;version=2, */*" => true,
                "text/event-stream;" => true,
                // The served representation has no parameters, so only the bare range applies.
                "text/event-stream;charset=utf-8;q=0, text/event-stream" => true,
                r#"text/event-stream;profile="a;b,c", */*"# => true,
            }
            "explicit exclusion or invalid weight" {
                "application/json" => false,
                "text/event-stream;q=0, */*;q=1" => false,
                "text/event-stream;q=0, text/*" => false,
                "text/event-stream;q=NaN" => false,
                r#"application/json;profile="a,text/event-stream,b", text/event-stream;q=0"# => false,
            }
        );
    }
}
