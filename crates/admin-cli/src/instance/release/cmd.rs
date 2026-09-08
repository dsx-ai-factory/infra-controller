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

use ::rpc::forge::{BatchInstanceReleaseResponse, InstanceReleaseRequest};
use carbide_uuid::instance::InstanceId;

use super::args::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// gRPC metadata key the API attaches to a `RESOURCE_EXHAUSTED` admission
/// rejection, carrying the advertised backoff in whole milliseconds. Must match
/// `GRPC_RETRY_PUSHBACK_HEADER` in `api-core/src/admission/mod.rs`.
const ADMISSION_RETRY_PUSHBACK_HEADER: &str = "grpc-retry-pushback-ms";
/// Attempt cap for the single batch RPC call, bounding how many times a
/// completed `RESOURCE_EXHAUSTED` rejection is retried. This does NOT bound
/// how long any single in-flight attempt can take: neither this loop nor the
/// underlying gRPC client sets a per-request deadline, so a connection that
/// hangs mid-request (accepted but never responds) can still block this
/// command indefinitely regardless of this cap.
const MAX_BATCH_ATTEMPTS: usize = 8;
/// Cumulative backoff cap across retries of the one batch call; give up past
/// this rather than retrying indefinitely.
const MAX_TOTAL_BACKOFF: Duration = Duration::from_secs(120);
/// Backoff used when the server omits an (unexpected) parseable pushback value.
const DEFAULT_ADMISSION_BACKOFF: Duration = Duration::from_secs(5);
/// Bounds mirroring the server's own advertised range in `admission/retry.rs`.
const MIN_ADMISSION_BACKOFF: Duration = Duration::from_secs(1);
const MAX_ADMISSION_BACKOFF: Duration = Duration::from_secs(30);
/// Attempt cap for the one-off preflight lookup that resolves `--label-key`/
/// `--machine` into a concrete instance list, mirroring [`MAX_BATCH_ATTEMPTS`].
const MAX_PREFLIGHT_LOOKUP_ATTEMPTS: usize = 8;

/// Outcome of parsing the server-advertised `grpc-retry-pushback-ms` header.
enum PushbackAdvice {
    /// No header present -- the caller should fall back to its own default.
    Absent,
    /// A valid non-negative delay was advertised.
    Delay(Duration),
    /// The header was present but negative or otherwise unparseable. Per the
    /// gRPC retry-pushback spec, this is an explicit "do not retry" signal
    /// from the server, distinct from simply omitting the header -- treating
    /// it the same as `Absent` (and retrying anyway with a default delay)
    /// would ignore the server's request to stop.
    StopRetrying,
}

/// Parses the server-advertised retry delay from a rejection's metadata.
fn admission_retry_delay(status: &tonic::Status) -> PushbackAdvice {
    let Some(raw) = status.metadata().get(ADMISSION_RETRY_PUSHBACK_HEADER) else {
        return PushbackAdvice::Absent;
    };
    let Ok(raw) = raw.to_str() else {
        return PushbackAdvice::StopRetrying;
    };
    match raw.parse::<i64>() {
        Ok(millis) if millis >= 0 => PushbackAdvice::Delay(Duration::from_millis(millis as u64)),
        // Negative (explicit stop signal) or unparseable -- both mean "stop".
        _ => PushbackAdvice::StopRetrying,
    }
}

/// Resolves the delay to sleep for one retry attempt, given a
/// `RESOURCE_EXHAUSTED` rejection. Returns `None` if the server signaled to
/// stop retrying (a negative or malformed pushback value), in which case the
/// caller should surface the error immediately rather than retry.
fn resolve_backoff_delay(status: &tonic::Status) -> Option<Duration> {
    match admission_retry_delay(status) {
        PushbackAdvice::Absent => Some(DEFAULT_ADMISSION_BACKOFF),
        PushbackAdvice::Delay(delay) => {
            Some(delay.clamp(MIN_ADMISSION_BACKOFF, MAX_ADMISSION_BACKOFF))
        }
        PushbackAdvice::StopRetrying => None,
    }
}

/// Retries a fallible preflight lookup call on `RESOURCE_EXHAUSTED` admission
/// rejections, honoring the server's advertised `grpc-retry-pushback-ms`
/// backoff exactly like [`release_batch_with_retry`] does for the batch
/// release call itself. Any other error surfaces immediately so real failures
/// are not masked.
///
/// Motivation: resolving `--label-key`/`--machine` into a concrete instance
/// list (`get_all_instances` / `find_instance_by_machine_id`) happens *before*
/// the batch release call. Without this, a `RESOURCE_EXHAUSTED` rejection on
/// that single preflight call aborted the whole release command with zero
/// instances attempted, even though the batch call itself was already
/// retry-safe -- observed live at ~4,500-instance scale, where under
/// sustained admission pressure some release invocations failed instantly
/// with no progress purely because this one lookup call happened to land in a
/// saturated window.
async fn retry_lookup_on_admission_exhaustion<T, F, Fut>(mut attempt: F) -> CarbideCliResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = CarbideCliResult<T>>,
{
    let mut total_backoff = Duration::ZERO;
    for attempt_number in 1..=MAX_PREFLIGHT_LOOKUP_ATTEMPTS {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(CarbideCliError::ApiInvocationError(status))
                if status.code() == tonic::Code::ResourceExhausted =>
            {
                if attempt_number == MAX_PREFLIGHT_LOOKUP_ATTEMPTS {
                    return Err(CarbideCliError::ApiInvocationError(status));
                }
                let Some(delay) = resolve_backoff_delay(&status) else {
                    return Err(CarbideCliError::ApiInvocationError(status));
                };
                if total_backoff.saturating_add(delay) > MAX_TOTAL_BACKOFF {
                    return Err(CarbideCliError::ApiInvocationError(status));
                }
                total_backoff = total_backoff.saturating_add(delay);
                tokio::time::sleep(delay).await;
            }
            Err(other) => return Err(other),
        }
    }
    unreachable!("loop returns on the final attempt")
}

/// Runs the single batch-release call, retrying only `RESOURCE_EXHAUSTED`
/// admission rejections of the call itself by honoring the server's advertised
/// `grpc-retry-pushback-ms` backoff. Retries are bounded by attempt count and
/// cumulative backoff; any other error surfaces immediately so real failures
/// are not masked.
///
/// Per-instance retry/backoff no longer lives here: the server-side
/// `ReleaseInstances` RPC releases the whole batch in one call (best-effort
/// per instance, see the RPC's proto doc comment), so there is only ever one
/// call to retry, not one per instance.
async fn release_batch_with_retry<F, Fut>(
    mut attempt: F,
) -> Result<BatchInstanceReleaseResponse, tonic::Status>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<BatchInstanceReleaseResponse, tonic::Status>>,
{
    let mut total_backoff = Duration::ZERO;
    for attempt_number in 1..=MAX_BATCH_ATTEMPTS {
        match attempt().await {
            Ok(response) => return Ok(response),
            Err(status) if status.code() == tonic::Code::ResourceExhausted => {
                if attempt_number == MAX_BATCH_ATTEMPTS {
                    return Err(status);
                }
                let Some(delay) = resolve_backoff_delay(&status) else {
                    return Err(status);
                };
                if total_backoff.saturating_add(delay) > MAX_TOTAL_BACKOFF {
                    return Err(status);
                }
                total_backoff = total_backoff.saturating_add(delay);
                tokio::time::sleep(delay).await;
            }
            Err(status) => return Err(status),
        }
    }
    unreachable!("loop returns on the final attempt")
}

pub(super) async fn release(
    api_client: &ApiClient,
    release_request: Args,
    ctx: &RuntimeContext,
) -> CarbideCliResult<()> {
    ctx.assert_cloud_unsafe_op_message()?;

    let mut instance_ids: Vec<InstanceId> = Vec::new();

    match (
        release_request.instance,
        release_request.machine,
        release_request.label_key,
    ) {
        (Some(instance_id), _, _) => instance_ids.push(
            uuid::Uuid::parse_str(&instance_id)
                .map_err(|e| CarbideCliError::GenericError(e.to_string()))?
                .into(),
        ),
        (_, Some(machine_id), _) => {
            let instances = retry_lookup_on_admission_exhaustion(|| async move {
                api_client
                    .0
                    .find_instance_by_machine_id(machine_id)
                    .await
                    .map_err(CarbideCliError::from)
            })
            .await?
            .instances;
            let Some(instance_id) = instances.into_iter().next().and_then(|i| i.id) else {
                return Err(CarbideCliError::GenericError(
                    "No instances assigned to that machine".to_string(),
                ));
            };
            instance_ids.push(instance_id);
        }
        (_, _, Some(key)) => {
            let instances = retry_lookup_on_admission_exhaustion(|| {
                api_client.get_all_instances(
                    None,
                    None,
                    Some(key.clone()),
                    release_request.label_value.clone(),
                    None,
                    ctx.config.page_size,
                )
            })
            .await?;
            if instances.instances.is_empty() {
                return Err(CarbideCliError::GenericError(
                    "No instances with the passed label.key exist".to_string(),
                ));
            }
            instance_ids = instances
                .instances
                .iter()
                .filter_map(|instance| instance.id)
                .collect();
        }
        _ => {}
    };
    let total = instance_ids.len();

    let release_requests: Vec<InstanceReleaseRequest> = instance_ids
        .into_iter()
        .map(|instance_id| InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        })
        .collect();

    let response = release_batch_with_retry(|| {
        let release_requests = release_requests.clone();
        async move {
            api_client
                .0
                .release_instances(::rpc::forge::BatchInstanceReleaseRequest { release_requests })
                .await
        }
    })
    .await
    .map_err(CarbideCliError::from)?;

    summarize_release_response(total, response)
}

/// Reports the outcome of a batch release call and turns a partial or failed
/// release into an `Err`. Derives its counts entirely from the response, not
/// from `total` (the request size): today's server always returns exactly
/// one result per request (see `batch_release` in
/// `crates/api-core/src/handlers/instance.rs`), but nothing in the proto
/// schema enforces that cardinality, so counting from the response guards
/// against a differently-behaved server (or a future bug) silently returning
/// fewer results than requested. For a destructive operation at scale,
/// silently treating a short response as a full success is the wrong
/// direction to be wrong in.
fn summarize_release_response(
    total: usize,
    response: BatchInstanceReleaseResponse,
) -> CarbideCliResult<()> {
    let results = response.results;
    let released = results
        .iter()
        .filter(|r| r.status == ::rpc::forge::InstanceReleaseStatusCode::Success as i32)
        .count();
    let failures: Vec<_> = results
        .iter()
        .filter(|r| r.status != ::rpc::forge::InstanceReleaseStatusCode::Success as i32)
        .collect();

    if failures.is_empty() && released == total {
        tracing::info!("Released {released} instance(s).");
        return Ok(());
    }

    if failures.is_empty() {
        tracing::error!("Released {released}/{total} instance(s); the rest were not confirmed.");
        return Err(CarbideCliError::GenericError(format!(
            "release incomplete: {released} of {total} instance(s) confirmed released"
        )));
    }

    tracing::error!(
        "Released {released}/{total} instance(s); {} failed:",
        failures.len()
    );
    for failure in &failures {
        tracing::error!(
            instance_id = ?failure.id,
            status = ?failure.status,
            "  {}",
            failure.error
        );
    }
    Err(CarbideCliError::GenericError(format!(
        "release incomplete: {} failed of {total} instance(s) (see logs above)",
        failures.len()
    )))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tonic::metadata::MetadataValue;

    use super::*;

    fn exhausted(pushback_millis: u64) -> tonic::Status {
        let mut status = tonic::Status::resource_exhausted("API admission capacity exhausted");
        status.metadata_mut().insert(
            ADMISSION_RETRY_PUSHBACK_HEADER,
            MetadataValue::try_from(pushback_millis.to_string().as_str()).unwrap(),
        );
        status
    }

    fn pushback_status(raw: &str) -> tonic::Status {
        let mut status = tonic::Status::resource_exhausted("API admission capacity exhausted");
        status.metadata_mut().insert(
            ADMISSION_RETRY_PUSHBACK_HEADER,
            MetadataValue::try_from(raw).unwrap(),
        );
        status
    }

    fn instance_ids(count: u128) -> Vec<InstanceId> {
        (1..=count)
            .map(|n| uuid::Uuid::from_u128(n).into())
            .collect()
    }

    fn success_outcome(id: InstanceId) -> ::rpc::forge::InstanceReleaseOutcome {
        ::rpc::forge::InstanceReleaseOutcome {
            id: Some(id),
            status: ::rpc::forge::InstanceReleaseStatusCode::Success as i32,
            error: String::new(),
        }
    }

    fn failed_outcome(id: Option<InstanceId>, error: &str) -> ::rpc::forge::InstanceReleaseOutcome {
        ::rpc::forge::InstanceReleaseOutcome {
            id,
            status: ::rpc::forge::InstanceReleaseStatusCode::NotFound as i32,
            error: error.to_string(),
        }
    }

    fn ok_response(ids: &[InstanceId]) -> BatchInstanceReleaseResponse {
        BatchInstanceReleaseResponse {
            results: ids.iter().copied().map(success_outcome).collect(),
        }
    }

    #[test]
    fn summarize_reports_success_when_all_requested_ids_are_released() {
        let ids = instance_ids(3);
        assert!(summarize_release_response(ids.len(), ok_response(&ids)).is_ok());
    }

    #[test]
    fn summarize_is_an_error_when_response_omits_ids_without_reporting_failures() {
        // Today's server always returns one result per request, but nothing
        // in the proto schema enforces that; this guards against a
        // differently-behaved server, so the CLI must not treat a short
        // response as a full success just because none of the returned
        // results are failures.
        let ids = instance_ids(3);
        let response = BatchInstanceReleaseResponse {
            results: ids[..2].iter().copied().map(success_outcome).collect(),
        };
        assert!(summarize_release_response(ids.len(), response).is_err());
    }

    #[test]
    fn summarize_is_an_error_when_there_are_reported_failures() {
        let ids = instance_ids(2);
        let response = BatchInstanceReleaseResponse {
            results: vec![
                success_outcome(ids[0]),
                failed_outcome(Some(ids[1]), "not found"),
            ],
        };
        assert!(summarize_release_response(ids.len(), response).is_err());
    }

    #[test]
    fn parses_advertised_pushback_delay() {
        assert!(matches!(
            admission_retry_delay(&exhausted(7_000)),
            PushbackAdvice::Delay(d) if d == Duration::from_secs(7)
        ));
        assert!(matches!(
            admission_retry_delay(&tonic::Status::resource_exhausted("no header")),
            PushbackAdvice::Absent
        ));
    }

    #[test]
    fn negative_pushback_is_a_stop_retrying_signal() {
        assert!(matches!(
            admission_retry_delay(&pushback_status("-1")),
            PushbackAdvice::StopRetrying
        ));
    }

    #[test]
    fn malformed_pushback_is_a_stop_retrying_signal() {
        assert!(matches!(
            admission_retry_delay(&pushback_status("not-a-number")),
            PushbackAdvice::StopRetrying
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_after_advertised_delay_then_succeeds() {
        let ids = instance_ids(3);
        let attempts = Cell::new(0);
        let start = tokio::time::Instant::now();

        let result = release_batch_with_retry(|| {
            let attempt = attempts.get() + 1;
            attempts.set(attempt);
            let ids = ids.clone();
            async move {
                if attempt < 3 {
                    Err(exhausted(7_000))
                } else {
                    Ok(ok_response(&ids))
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(attempts.get(), 3);
        // Two rejections, each honoring the advertised 7s pushback.
        assert_eq!(start.elapsed(), Duration::from_secs(14));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_are_bounded_by_attempt_cap() {
        let attempts = Cell::new(0);

        let result = release_batch_with_retry(|| {
            attempts.set(attempts.get() + 1);
            async move { Err::<BatchInstanceReleaseResponse, _>(exhausted(1_000)) }
        })
        .await;

        assert_eq!(result.unwrap_err().code(), tonic::Code::ResourceExhausted);
        assert_eq!(attempts.get(), MAX_BATCH_ATTEMPTS);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_immediately_on_negative_pushback() {
        let attempts = Cell::new(0);

        let result = release_batch_with_retry(|| {
            attempts.set(attempts.get() + 1);
            async move { Err::<BatchInstanceReleaseResponse, _>(pushback_status("-1")) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn non_admission_errors_surface_without_retry() {
        let attempts = Cell::new(0);

        let result = release_batch_with_retry(|| {
            attempts.set(attempts.get() + 1);
            async move { Err::<BatchInstanceReleaseResponse, _>(tonic::Status::not_found("gone")) }
        })
        .await;

        assert_eq!(result.unwrap_err().code(), tonic::Code::NotFound);
        assert_eq!(attempts.get(), 1);
    }
}
