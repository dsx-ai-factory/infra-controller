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

use std::future::Future;
use std::time::Duration;

use ::rpc::protos::{forge as rpc, mlx_device};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::CarbideError;
use crate::api::{Api, ScoutStreamType, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

// Keep this hard-coded unless an operational need for tuning is demonstrated.
// Scout sends Init immediately after opening the RPC, so ten seconds is generous
// for a healthy connection and serves as a protocol safety bound rather than a
// deployment-specific policy. Making it configurable prematurely would add
// configuration surface and allow this resource-leak protection to be weakened.
const SCOUT_STREAM_INIT_TIMEOUT: Duration = Duration::from_secs(10);

// Snapshot writes share the Scout receive loop with every other response.
// Keep attempts inline to avoid spawning an unbounded number of database tasks.
// Give each best-effort write at most one second, including pool acquisition,
// so an unavailable database cannot hold up the connection for its full timeout.
const MLX_OBSERVATION_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

// scout_stream handles the bidirectional streaming connection from scout agents.
// scout agents call scout_stream and send an Init message, and then carbide-api
// will send down "request" messages to connected agent(s) to either instruct them
// or ask them for information (sometimes for state changes, other times for
// feeding data back to administrative CLI/UI calls).
pub(crate) async fn scout_stream(
    api: &Api,
    request: Request<Streaming<rpc::ScoutStreamApiBoundMessage>>,
) -> Result<Response<ScoutStreamType>, Status> {
    log_request_data(&request);

    let authenticated_machine_id = super::mlx_device_report::authenticated_machine_id(&request)?;
    let mut stream = request.into_inner();

    let init_message = receive_initial_message(stream.message(), SCOUT_STREAM_INIT_TIMEOUT).await?;

    // As part of "constructing" the new scout stream, we expect
    // an Init message as the first thing from the client (in this
    // case, a scout agent).
    let machine_id = match init_message.payload {
        Some(rpc::scout_stream_api_bound_message::Payload::Init(init)) => {
            convert_and_log_machine_id(init.machine_id.as_ref())?
        }
        _ => {
            return Err(CarbideError::InvalidArgument(
                "first ScoutStream client message must be an init message".into(),
            )
            .into());
        }
    };

    if authenticated_machine_id.is_some_and(|authenticated| authenticated != machine_id) {
        return Err(CarbideError::PermissionDeniedError(
            "ScoutStream init machine ID does not match the authenticated source".into(),
        )
        .into());
    }

    tracing::info!(
        machine_id = %machine_id,
        "Scout agent connected",
    );

    // Now we create channels for bidirectional communication. The API
    // will receive on one side, process whatever is packed into the oneof field
    // for the stream message, and then pass it off out the other side.
    let (agent_tx, agent_rx) = mpsc::channel::<rpc::ScoutStreamApiBoundMessage>(100);
    let (server_tx, server_rx) =
        mpsc::channel::<Result<rpc::ScoutStreamScoutBoundMessage, Status>>(100);

    // Next, register the connection using the machine ID and our fancy new channels.
    api.scout_stream_registry
        .register(machine_id, server_tx.clone(), agent_rx)
        .await;

    // And now spawn a task to forward agent messages through
    // the connection registry.
    let registry_clone = api.scout_stream_registry.clone();
    let pool = api.database_connection.clone();
    tokio::spawn(async move {
        while let Ok(Some(message)) = stream.message().await {
            // This is an observation from the authenticated host, not proof
            // that an outstanding request or firmware operation completed.
            // Unbound simulator connections retain their existing behavior.
            if let Some(authenticated) = authenticated_machine_id
                && let Some(
                    rpc::scout_stream_api_bound_message::Payload::MlxDeviceInfoReportResponse(
                        mlx_device::MlxDeviceInfoReportResponse {
                            reply:
                                Some(mlx_device::mlx_device_info_report_response::Reply::DeviceReport(
                                    report,
                                )),
                        },
                    ),
                ) = message.payload.as_ref()
            {
                match tokio::time::timeout(
                    MLX_OBSERVATION_WRITE_TIMEOUT,
                    super::mlx_device_report::persist(&pool, authenticated, report),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(
                        machine_id = %authenticated,
                        observed_at = ?report.timestamp,
                        error = %error,
                        "Failed to retain MLX observation; forwarding Scout response"
                    ),
                    Err(error) => tracing::warn!(
                        machine_id = %authenticated,
                        observed_at = ?report.timestamp,
                        error = %error,
                        "Timed out waiting for MLX observation persistence; forwarding Scout response"
                    ),
                }
            }
            if agent_tx.send(message).await.is_err() {
                tracing::error!("failed to forward message received from scout agent");
                break;
            }
        }

        // If/when the connection breaks, unregister the scout
        // agent connection from the connection registry.
        tracing::info!(
            machine_id = %machine_id,
            "Scout agent disconnected",
        );
        registry_clone.unregister(machine_id).await;
    });

    // Ok(Response::new(ReceiverStream::new(server_rx)))
    Ok(Response::new(Box::pin(ReceiverStream::new(server_rx))))
}

async fn receive_initial_message<F>(
    message: F,
    timeout: Duration,
) -> Result<rpc::ScoutStreamApiBoundMessage, Status>
where
    F: Future<Output = Result<Option<rpc::ScoutStreamApiBoundMessage>, Status>>,
{
    match tokio::time::timeout(timeout, message).await {
        Ok(result) => result?.ok_or_else(|| {
            CarbideError::InvalidArgument("invalid message received".to_string()).into()
        }),
        Err(_) => Err(Status::deadline_exceeded(
            "timed out waiting for initial ScoutStream init message",
        )),
    }
}

pub(crate) async fn show_connections(
    api: &Api,
    request: Request<rpc::ScoutStreamShowConnectionsRequest>,
) -> Result<Response<rpc::ScoutStreamShowConnectionsResponse>, Status> {
    log_request_data(&request);

    let connections = api.scout_stream_registry.list_connected().await;

    let connection_list = connections
        .into_iter()
        .map(|(machine_id, connected_at)| {
            let duration = connected_at
                .elapsed()
                .unwrap_or(std::time::Duration::from_secs(0));

            rpc::ScoutStreamConnectionInfo {
                machine_id: machine_id.into(),
                connected_at: format_system_time(connected_at),
                uptime_seconds: duration.as_secs(),
            }
        })
        .collect();

    Ok(Response::new(rpc::ScoutStreamShowConnectionsResponse {
        scout_stream_connections: connection_list,
    }))
}
pub(crate) async fn disconnect(
    api: &Api,
    request: Request<rpc::ScoutStreamDisconnectRequest>,
) -> Result<Response<rpc::ScoutStreamDisconnectResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let success = api.scout_stream_registry.unregister(machine_id).await;
    Ok(Response::new(rpc::ScoutStreamDisconnectResponse {
        machine_id: machine_id.into(),
        success,
    }))
}

pub(crate) async fn ping(
    api: &Api,
    request: Request<rpc::ScoutStreamAdminPingRequest>,
) -> Result<Response<rpc::ScoutStreamAdminPingResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;

    // Check if the machine is connected.
    if !api.scout_stream_registry.is_connected(machine_id).await {
        return Err(CarbideError::NotFoundError {
            kind: "scout agent connection",
            id: machine_id.to_string(),
        }
        .into());
    }

    let request = rpc::ScoutStreamScoutBoundMessage::new_flow(
        rpc::scout_stream_scout_bound_message::Payload::ScoutStreamAgentPingRequest(
            rpc::ScoutStreamAgentPingRequest {},
        ),
    );

    let response = api
        .scout_stream_registry
        .send_request(machine_id, request)
        .await
        .map_err(|status| CarbideError::Internal {
            message: format!(
                "error while attempting to send ping request to scout: {}",
                status.message()
            ),
        })?;

    match response.payload {
        Some(rpc::scout_stream_api_bound_message::Payload::ScoutStreamAgentPingResponse(
            agent_ping_response,
        )) => match agent_ping_response.reply {
            Some(rpc::scout_stream_agent_ping_response::Reply::Pong(pong)) => {
                Ok(Response::new(rpc::ScoutStreamAdminPingResponse { pong }))
            }
            Some(rpc::scout_stream_agent_ping_response::Reply::Error(error)) => {
                Err(CarbideError::Internal {
                    message: format!(
                        "scout agent returned error attempting to ping agent (machine_id={machine_id}): {}",
                        error.message
                    ),
                }
                .into())
            }
            None => Err(CarbideError::Internal {
                message: format!(
                    "scout agent returned empty ping reply (machine_id={machine_id})"
                ),
            }
            .into()),
        },
        _ => Err(CarbideError::Internal {
            message: format!(
                "unexpected response type from scout agent for ping response (machine_id={machine_id})"
            ),
        }
        .into()),
    }
}

// format_system_time formats a SystemTime as an RFC3339 string.
fn format_system_time(time: std::time::SystemTime) -> String {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => {
            let secs = duration.as_secs();
            chrono::DateTime::from_timestamp(secs as i64, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_string())
        }
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;

    use ::rpc::forge::forge_server::ForgeServer;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as AxumRequest;
    use axum::routing::post;
    use futures::stream;
    use tokio::sync::Notify;
    use tokio::task::JoinSet;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::*;
    use crate::admission::{ApiAdmissionControl, enforce as enforce_admission};
    use crate::cfg::file::ApiAdmissionControlConfig;
    use crate::tests::create_test_env;

    #[test]
    fn stalled_initial_message_times_out_and_releases_admission_capacity() {
        let mut args = sqlx::testing::TestArgs::new(concat!(
            module_path!(),
            "::stalled_initial_message_times_out_and_releases_admission_capacity"
        ));
        args.fixtures(Box::leak(Box::new(vec![])));
        let test_fn: fn(sqlx::PgPool) -> _ = stalled_initial_message_test_body;
        // The unoptimized generated Forge dispatcher alone consumes nearly 2 MiB
        // of stack as RPCs are added. Keep the real router test on a dedicated
        // stack without changing production runtime settings or test assertions.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || sqlx_testing::TestFn::run_test(test_fn, args))
            .expect("spawn scout stream router test")
            .join()
            .expect("scout stream router test panicked");
    }

    async fn stalled_initial_message_test_body(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let mut join_set = JoinSet::new();
        let controller = ApiAdmissionControl::from_config(
            &ApiAdmissionControlConfig {
                enabled: true,
                max_work_in_flight: 1,
                max_pending: 1,
                max_work_in_flight_per_client: 1,
                max_pending_per_client: 1,
                pending_timeout: SCOUT_STREAM_INIT_TIMEOUT + Duration::from_secs(1),
                client_idle_timeout: Duration::from_secs(60),
                service_limits: Default::default(),
            },
            &opentelemetry::global::meter("scout-stream-init-timeout-test"),
            CancellationToken::new(),
            &mut join_set,
        )
        .expect("test admission config is valid")
        .expect("test admission is enabled");
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let probe_handler = {
            let probe_calls = Arc::clone(&probe_calls);
            move || {
                let probe_calls = Arc::clone(&probe_calls);
                async move {
                    probe_calls.fetch_add(1, Ordering::SeqCst);
                    "probe response"
                }
            }
        };
        let router = Router::new()
            .route_service(
                ::rpc::service_path!("{*rpc}"),
                ForgeServer::from_arc(Arc::clone(&env.api)),
            )
            .route("/admin/probe", post(probe_handler))
            .layer(axum::middleware::from_fn_with_state(
                controller,
                enforce_admission,
            ));

        tokio::time::pause();
        let stalled_message_polled = Arc::new(Notify::new());
        let pending_body = {
            let stalled_message_polled = Arc::clone(&stalled_message_polled);
            Body::from_stream(stream::poll_fn(move |_| {
                stalled_message_polled.notify_one();
                Poll::<Option<Result<String, Infallible>>>::Pending
            }))
        };
        let stalled_router = router.clone();
        let stalled_request = tokio::spawn(async move {
            stalled_router
                .oneshot(
                    AxumRequest::post(::rpc::service_path!("ScoutStream"))
                        .header(axum::http::header::CONTENT_TYPE, "application/grpc")
                        .header("te", "trailers")
                        .body(pending_body)
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        stalled_message_polled.notified().await;

        let probe_router = router.clone();
        let probe_request = tokio::spawn(async move {
            probe_router
                .oneshot(
                    AxumRequest::post("/admin/probe")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        assert_eq!(probe_calls.load(Ordering::SeqCst), 0);

        tokio::time::advance(SCOUT_STREAM_INIT_TIMEOUT + Duration::from_millis(1)).await;

        let stalled_response = stalled_request.await.unwrap();
        assert_eq!(stalled_response.status(), axum::http::StatusCode::OK);
        assert_eq!(stalled_response.headers().get("grpc-status").unwrap(), "4");

        let probe_response = probe_request.await.unwrap();
        assert_eq!(probe_response.status(), axum::http::StatusCode::OK);
        assert_eq!(probe_calls.load(Ordering::SeqCst), 1);
        tokio::time::resume();
    }
}
