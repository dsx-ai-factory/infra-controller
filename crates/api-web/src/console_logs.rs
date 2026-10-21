/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::machine::MachineId;
use futures::StreamExt;
use rpc::protos::console_log::StreamConsoleLogsRequest;
use rpc::protos::forge::forge_server::Forge;
use serde::{Serialize, Serializer};

use super::Base;

#[derive(Template)]
#[template(path = "console_logs.html")]
struct ConsoleLogsPage {
    machine_id: String,
}

impl Base for ConsoleLogsPage {}

pub(super) async fn page(Path(machine_id): Path<String>) -> Response {
    if machine_id.parse::<MachineId>().is_err() {
        return (StatusCode::BAD_REQUEST, "invalid machine ID").into_response();
    }
    Html(ConsoleLogsPage { machine_id }.render().unwrap()).into_response()
}

pub(super) async fn stream(
    State(api): State<Arc<Api>>,
    Path(machine_id): Path<String>,
) -> Response {
    let machine_id = match machine_id.parse::<MachineId>() {
        Ok(machine_id) => machine_id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid machine ID").into_response(),
    };
    let upstream = match api
        .stream_console_logs(tonic::Request::new(StreamConsoleLogsRequest {
            machine_id: Some(machine_id),
            tail_lines: 1000,
        }))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(status) => return ErrorEvent::from(status).into_response(),
    };

    let events = futures::stream::unfold(Some(upstream), |state| async move {
        let mut upstream = state?;
        match upstream.next().await {
            Some(Ok(line)) => {
                let text = String::from_utf8_lossy(&line.data);
                let data = serde_json::to_string(text.as_ref()).expect("string JSON encoding");
                Some((Ok(Event::default().data(data)), Some(upstream)))
            }
            Some(Err(status)) => Some((
                Event::default()
                    .event("console-error")
                    .json_data(ErrorEvent::from(status)),
                None,
            )),
            None => None,
        }
    });

    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[derive(Serialize)]
struct ErrorEvent {
    #[serde(serialize_with = "serialize_status_code")]
    code: StatusCode,
    message: String,
}

impl IntoResponse for ErrorEvent {
    fn into_response(self) -> Response {
        (self.code, self.message).into_response()
    }
}

fn serialize_status_code<S>(status_code: &StatusCode, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u16(status_code.as_u16())
}

impl From<tonic::Status> for ErrorEvent {
    fn from(status: tonic::Status) -> Self {
        let code = match status.code() {
            tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
            tonic::Code::NotFound => StatusCode::NOT_FOUND,
            tonic::Code::FailedPrecondition => StatusCode::PRECONDITION_FAILED,
            tonic::Code::PermissionDenied => StatusCode::FORBIDDEN,
            tonic::Code::Unauthenticated => StatusCode::UNAUTHORIZED,
            tonic::Code::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match status.code() {
            tonic::Code::Internal | tonic::Code::Unknown => "console log stream failed",
            tonic::Code::Unavailable => "console log service unavailable",
            _ => status.message(),
        };

        Self {
            code,
            message: message.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_is_text_safe_and_bounded() {
        let html = ConsoleLogsPage {
            machine_id: "m-test".to_string(),
        }
        .render()
        .unwrap();
        assert!(html.contains("output.textContent"));
        assert!(html.contains("lines.length > 5000"));
        assert!(!html.contains("innerHTML"));
    }

    #[test]
    fn grpc_status_maps_to_initial_http_status() {
        assert_eq!(
            ErrorEvent::from(tonic::Status::not_found("missing")).code,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ErrorEvent::from(tonic::Status::unavailable("down")).code,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
