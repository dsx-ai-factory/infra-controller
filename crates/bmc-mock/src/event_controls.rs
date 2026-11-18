// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Mock-only HTTP controls for the `EventService`, under `/Mock/EventService`
//! beside the Redfish tree they manipulate, like `/ipmi`. They delegate to
//! `EventServiceState` and inherit the BMC's authentication.

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::redfish::event_service::{EventServiceError, enabled};
use crate::sse::StreamStep;
use crate::{BmcState, Callbacks};

pub(super) fn add_routes<C: Callbacks>(router: Router<BmcState<C>>) -> Router<BmcState<C>> {
    router
        .route("/Mock/EventService/events", post(publish::<C>))
        .route("/Mock/EventService/stats", get(stats::<C>))
        .route("/Mock/EventService/close", post(close::<C>))
        .route(
            "/Mock/EventService/scripts",
            post(script::<C>).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
}

async fn publish<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Json(payload): Json<Value>,
) -> Result<Response, EventServiceError> {
    let id = enabled(&state)?.publish(payload)?;
    Ok(Json(json!({"id": id})).into_response())
}

async fn stats<C: Callbacks>(
    State(state): State<BmcState<C>>,
) -> Result<Response, EventServiceError> {
    Ok(Json(enabled(&state)?.stats()).into_response())
}

async fn close<C: Callbacks>(
    State(state): State<BmcState<C>>,
) -> Result<Response, EventServiceError> {
    enabled(&state)?.close_subscribers();
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn script<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Json(steps): Json<Vec<StreamStep>>,
) -> Result<Response, EventServiceError> {
    enabled(&state)?.queue_script(steps)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use nv_redfish::event_service::EventStreamPayload;
    use serde_json::json;

    use crate::redfish::event_service::fixtures::{event, metric, request, router};

    #[tokio::test]
    async fn publication_uses_consumer_payload_discriminator() {
        let (router, bmc) = router(false);
        for (scenario, (kind, mut payload), expected) in [
            (
                "OEM namespace",
                ("#Oem.Nvidia.Event", event()),
                StatusCode::OK,
            ),
            (
                "unqualified type accepted by consumer",
                ("#Event", event()),
                StatusCode::OK,
            ),
            (
                "final type segment controls decoding",
                ("#Event.v1_0_0.MetricReport", metric()),
                StatusCode::OK,
            ),
        ] {
            let actual = {
                let router = router.clone();
                {
                    payload["@odata.type"] = json!(kind);
                    assert!(serde_json::from_value::<EventStreamPayload>(payload.clone()).is_ok());

                    request(&router, "POST", "/Mock/EventService/events", Some(payload))
                        .await
                        .status()
                }
            };
            assert_eq!(actual, expected, "{scenario}");
        }
        assert_eq!(
            bmc.event_service.as_ref().unwrap().stats().retained_frames,
            3
        );
    }
}
