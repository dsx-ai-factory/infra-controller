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

//! Client-side handling of the `grpc-retry-pushback-ms` metadata nico-api
//! attaches to a `RESOURCE_EXHAUSTED` admission rejection.

use std::time::Duration;

/// gRPC metadata key the API attaches to a `RESOURCE_EXHAUSTED` admission
/// rejection, carrying the advertised backoff in whole milliseconds. Must
/// match `GRPC_RETRY_PUSHBACK_HEADER` in `api-core/src/admission/mod.rs`.
pub const ADMISSION_RETRY_PUSHBACK_HEADER: &str = "grpc-retry-pushback-ms";
/// Backoff used when the server omits an (unexpected) parseable pushback value.
pub const DEFAULT_ADMISSION_BACKOFF: Duration = Duration::from_secs(5);
/// Bounds mirroring the server's own advertised range in `admission/retry.rs`.
pub const MIN_ADMISSION_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_ADMISSION_BACKOFF: Duration = Duration::from_secs(30);

/// Outcome of parsing the server-advertised `grpc-retry-pushback-ms` header.
pub enum PushbackAdvice {
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
pub fn admission_retry_delay(status: &tonic::Status) -> PushbackAdvice {
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
pub fn resolve_backoff_delay(status: &tonic::Status) -> Option<Duration> {
    match admission_retry_delay(status) {
        PushbackAdvice::Absent => Some(DEFAULT_ADMISSION_BACKOFF),
        PushbackAdvice::Delay(delay) => {
            Some(delay.clamp(MIN_ADMISSION_BACKOFF, MAX_ADMISSION_BACKOFF))
        }
        PushbackAdvice::StopRetrying => None,
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn resolve_backoff_delay_clamps_and_defaults() {
        assert_eq!(
            resolve_backoff_delay(&tonic::Status::resource_exhausted("no header")),
            Some(DEFAULT_ADMISSION_BACKOFF)
        );
        assert_eq!(
            resolve_backoff_delay(&exhausted(1)),
            Some(MIN_ADMISSION_BACKOFF)
        );
        assert_eq!(
            resolve_backoff_delay(&exhausted(60_000)),
            Some(MAX_ADMISSION_BACKOFF)
        );
        assert_eq!(resolve_backoff_delay(&pushback_status("-1")), None);
    }
}
