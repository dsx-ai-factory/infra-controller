// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::future::Future;

use tonic::{Request, Response, Status};
use trace_propagation::correlation::{HEADER, scope};

/// Preserve the originating trace ID for logs throughout queued CM work.
/// The ID is correlation metadata, not an authorization or idempotency token.
pub(super) async fn run<T, R, F>(
    request: Request<T>,
    handler: impl FnOnce(Request<T>) -> F,
) -> Result<Response<R>, Status>
where
    F: Future<Output = Result<Response<R>, Status>>,
{
    let trace_id =
        trace_propagation::correlation::from_headers(&request.metadata().clone().into_headers());
    scope(trace_id, async move {
        tracing::info!("CM operation received");
        // Do not inline large CM handler futures into every enclosing request future.
        let result = Box::pin(handler(request)).await;
        match result {
            Ok(mut response) => {
                response.metadata_mut().insert(
                    HEADER,
                    trace_id
                        .to_string()
                        .parse()
                        .expect("a validated hexadecimal trace ID is valid gRPC metadata"),
                );
                // Success here is the RPC outcome, not completion of queued work.
                tracing::info!("CM operation RPC succeeded");
                Ok(response)
            }
            Err(mut status) => {
                status.metadata_mut().insert(
                    HEADER,
                    trace_id
                        .to_string()
                        .parse()
                        .expect("a validated hexadecimal trace ID is valid gRPC metadata"),
                );
                tracing::warn!(grpc_status_code = ?status.code(), "CM operation RPC failed");
                Err(status)
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn large_handler_future_does_not_expand_rpc_wrapper() {
        let future = run(Request::new(()), |_| async {
            let payload = std::hint::black_box([0_u8; 8 * 1024]);
            tokio::task::yield_now().await;
            Ok(Response::new(std::hint::black_box(payload)[0]))
        });
        assert!(
            std::mem::size_of_val(&future) < 4096,
            "RPC wrapper must not embed the handler's large future"
        );
        assert_eq!(future.await.unwrap().into_inner(), 0);
    }

    #[tokio::test]
    async fn invalid_log_headers_do_not_reject_the_rpc() {
        for values in [vec!["invalid"], vec!["invalid", "invalid"]] {
            let mut request = Request::new(());
            for value in values {
                request
                    .metadata_mut()
                    .append(HEADER, value.parse().unwrap());
            }
            let response = run(request, |_| async {
                assert!(trace_propagation::correlation::current_id().is_some());
                Ok(Response::new("executed"))
            })
            .await
            .unwrap();
            assert!(response.metadata().contains_key(HEADER));
            assert_eq!(response.into_inner(), "executed");
        }
    }

    #[tokio::test]
    async fn cm_rpc_preserves_incoming_trace_id_on_success_and_error() {
        let id = "1234567890abcdef1234567890abcdef";
        for succeeds in [true, false] {
            let mut request = Request::new(());
            request.metadata_mut().insert(
                "traceparent",
                format!("00-{id}-1234567890abcdef-00").parse().unwrap(),
            );
            let result = run(request, |_| async {
                assert_eq!(
                    trace_propagation::correlation::current_id()
                        .unwrap()
                        .to_string(),
                    id
                );
                if succeeds {
                    Ok(Response::new(()))
                } else {
                    Err(Status::unavailable("backend unavailable"))
                }
            })
            .await;
            match result {
                Ok(response) => {
                    assert!(succeeds);
                    assert_eq!(
                        response.metadata().get(HEADER).unwrap().to_str().unwrap(),
                        id
                    );
                }
                Err(status) => {
                    assert!(!succeeds);
                    assert_eq!(status.code(), tonic::Code::Unavailable);
                    assert_eq!(status.metadata().get(HEADER).unwrap().to_str().unwrap(), id);
                }
            }
            assert!(trace_propagation::correlation::current_id().is_none());
        }
    }
}
