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

use crate::BmcState;
use crate::redfish::event_service::{EventServiceError, enabled};
use crate::sse::StreamStep;

pub(super) fn add_routes(router: Router<BmcState>) -> Router<BmcState> {
    router
        .route("/Mock/EventService/events", post(publish))
        .route("/Mock/EventService/stats", get(stats))
        .route("/Mock/EventService/close", post(close))
        .route(
            "/Mock/EventService/scripts",
            post(script).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
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

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use nv_redfish::event_service::EventStreamPayload;
    use serde_json::json;

    use crate::redfish::event_service::fixtures::{event, metric, request, router};

    #[tokio::test]
    async fn publication_uses_consumer_payload_discriminator() {
        let (router, bmc) = router(false);
        check_cases_async(
            [
                Case {
                    scenario: "OEM namespace",
                    input: ("#Oem.Nvidia.Event", event()),
                    expect: Yields(StatusCode::OK),
                },
                Case {
                    scenario: "unqualified type accepted by consumer",
                    input: ("#Event", event()),
                    expect: Yields(StatusCode::OK),
                },
                Case {
                    scenario: "final type segment controls decoding",
                    input: ("#Event.v1_0_0.MetricReport", metric()),
                    expect: Yields(StatusCode::OK),
                },
            ],
            |(kind, mut payload)| {
                let router = router.clone();
                async move {
                    payload["@odata.type"] = json!(kind);
                    assert!(serde_json::from_value::<EventStreamPayload>(payload.clone()).is_ok());
                    Ok::<_, std::convert::Infallible>(
                        request(&router, "POST", "/Mock/EventService/events", Some(payload))
                            .await
                            .status(),
                    )
                }
            },
        )
        .await;
        assert_eq!(
            bmc.event_service.as_ref().unwrap().stats().retained_frames,
            3
        );
    }
}
