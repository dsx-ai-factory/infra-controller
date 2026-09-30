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

//! Retry loop for gRPC calls that get rejected with `RESOURCE_EXHAUSTED`
//! admission-control errors. Every retry loop in this crate that reacts to
//! `grpc-retry-pushback-ms` (release's preflight lookups, and
//! `get_all_instances`'s internal paged fetches) should go through
//! [`retry_on_admission_exhaustion`] rather than reimplementing the loop.
//! Pushback parsing itself lives in `rpc::admission_retry`, shared with
//! machine-a-tron. (`release_batch_with_retry` in `instance/release/cmd.rs`
//! is the one exception -- it retries a call returning
//! `Result<BatchInstanceReleaseResponse, tonic::Status>` rather than
//! `CarbideCliResult<T>`, so it calls [`resolve_backoff_delay`] directly but
//! keeps its own loop.)

use std::future::Future;
use std::time::Duration;

use ::rpc::admission_retry::resolve_backoff_delay;

use crate::errors::{CarbideCliError, CarbideCliResult};

/// Retries a fallible call on `RESOURCE_EXHAUSTED` admission rejections,
/// honoring the server's advertised `grpc-retry-pushback-ms` backoff (or
/// stopping immediately if the server signals not to retry -- see
/// [`resolve_backoff_delay`]). Bounded by `max_attempts` and
/// `max_total_backoff`; any other error surfaces immediately so real
/// failures are not masked.
///
/// Each call site picks its own `max_attempts`/`max_total_backoff` since the
/// right bound depends on what's being retried (a single lightweight lookup
/// vs. one page of a large paged fetch), but the parsing and stop-signal
/// handling stays in one place.
pub(crate) async fn retry_on_admission_exhaustion<T, F, Fut>(
    max_attempts: usize,
    max_total_backoff: Duration,
    mut attempt: F,
) -> CarbideCliResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = CarbideCliResult<T>>,
{
    // Never let a caller-supplied 0 reach the loop below: `1..=0` is empty, which would fall
    // through to the `unreachable!` -- a real panic path for a network-retry helper, not just
    // a formality, since this is `pub(crate)` and every current call site's `8` is a caller
    // choice, not an enforced invariant.
    let max_attempts = max_attempts.max(1);
    let mut total_backoff = Duration::ZERO;
    for attempt_number in 1..=max_attempts {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(CarbideCliError::ApiInvocationError(status))
                if status.code() == tonic::Code::ResourceExhausted =>
            {
                if attempt_number == max_attempts {
                    return Err(CarbideCliError::ApiInvocationError(status));
                }
                let Some(delay) = resolve_backoff_delay(&status) else {
                    return Err(CarbideCliError::ApiInvocationError(status));
                };
                if total_backoff.saturating_add(delay) > max_total_backoff {
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use ::rpc::admission_retry::ADMISSION_RETRY_PUSHBACK_HEADER;
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

    #[tokio::test(start_paused = true)]
    async fn retries_after_advertised_delay_then_succeeds() {
        let attempts = Cell::new(0);
        let start = tokio::time::Instant::now();

        let result = retry_on_admission_exhaustion(8, Duration::from_secs(120), || {
            let attempt = attempts.get() + 1;
            attempts.set(attempt);
            async move {
                if attempt < 3 {
                    Err(CarbideCliError::ApiInvocationError(exhausted(7_000)))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(attempts.get(), 3);
        assert_eq!(start.elapsed(), Duration::from_secs(14));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_are_bounded_by_attempt_cap() {
        let attempts = Cell::new(0);

        let result = retry_on_admission_exhaustion(8, Duration::from_secs(120), || {
            attempts.set(attempts.get() + 1);
            async move { Err::<(), _>(CarbideCliError::ApiInvocationError(exhausted(1_000))) }
        })
        .await;

        assert!(matches!(
            result.unwrap_err(),
            CarbideCliError::ApiInvocationError(status) if status.code() == tonic::Code::ResourceExhausted
        ));
        assert_eq!(attempts.get(), 8);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_immediately_on_negative_pushback() {
        let attempts = Cell::new(0);

        let result = retry_on_admission_exhaustion(8, Duration::from_secs(120), || {
            attempts.set(attempts.get() + 1);
            async move { Err::<(), _>(CarbideCliError::ApiInvocationError(pushback_status("-1"))) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn non_admission_errors_surface_without_retry() {
        let attempts = Cell::new(0);

        let result = retry_on_admission_exhaustion(8, Duration::from_secs(120), || {
            attempts.set(attempts.get() + 1);
            async move {
                Err::<(), _>(CarbideCliError::ApiInvocationError(
                    tonic::Status::not_found("gone"),
                ))
            }
        })
        .await;

        assert!(matches!(
            result.unwrap_err(),
            CarbideCliError::ApiInvocationError(status) if status.code() == tonic::Code::NotFound
        ));
        assert_eq!(attempts.get(), 1);
    }
}
