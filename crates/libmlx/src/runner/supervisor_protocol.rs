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

//! Private, bounded messages between libmlx and its matching supervisor binary.

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub(crate) const HELLO: &[u8] = b"MLX-SUPERVISOR-1\n";
// Retain at most 1 MiB per output stream so a noisy tool cannot grow memory
// indefinitely while still preserving useful vendor diagnostics.
pub(crate) const OUTPUT_LIMIT: usize = 1024 * 1024;
// Bound one encoded command to 128 KiB before allocating its argument payload.
pub(crate) const REQUEST_LIMIT: usize = 128 * 1024;
// JSON encodes each byte as at most three digits and a comma, in two streams.
pub(crate) const RESPONSE_LIMIT: usize = 8 * OUTPUT_LIMIT + 64 * 1024;
// Give killed descendants five seconds to be reaped, then report uncertainty
// instead of holding the caller indefinitely.
pub(crate) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
// A stalled result reader gets five seconds; completed cleanup must not leave
// the helper waiting forever to publish its response.
pub(crate) const RESULT_TIMEOUT: Duration = Duration::from_secs(5);

/// `ToolOwnerExitBehavior` controls what a closed request pipe means after dispatch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum ToolOwnerExitBehavior {
    /// Stop and reap the tool family when its owner disappears.
    #[default]
    Terminate,
    /// Keep running under the attempt deadline, including output and cleanup bounds.
    Continue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) timeout: Duration,
    /// Omission in protocol v1 selects `Terminate`; surviving owner exit
    /// requires an explicit `Continue` policy.
    #[serde(default)]
    pub(crate) owner_exit_behavior: ToolOwnerExitBehavior,
}

#[derive(Serialize, Deserialize)]
pub(crate) enum Response {
    NotStarted(IoFailure),
    Finished {
        status: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Failed {
        status: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        reason: Failure,
        cleanup_errors: Vec<IoFailure>,
    },
}

#[derive(Serialize, Deserialize)]
pub(crate) enum Failure {
    Deadline,
    Cancelled,
    OutputLimit,
    Io(IoFailure),
}

/// OS error numbers survive IPC; synthetic failures retain their useful category.
#[derive(Serialize, Deserialize)]
pub(crate) struct IoFailure {
    errno: Option<i32>,
    kind: IoFailureKind,
    message: String,
}

#[derive(Serialize, Deserialize)]
enum IoFailureKind {
    TimedOut,
    InvalidData,
    Other,
}

impl From<io::Error> for IoFailure {
    fn from(error: io::Error) -> Self {
        let message: String = error.to_string().chars().take(2048).collect();
        Self {
            errno: error.raw_os_error(),
            kind: match error.kind() {
                io::ErrorKind::TimedOut => IoFailureKind::TimedOut,
                io::ErrorKind::InvalidData => IoFailureKind::InvalidData,
                _ => IoFailureKind::Other,
            },
            message,
        }
    }
}

impl From<IoFailure> for io::Error {
    fn from(error: IoFailure) -> Self {
        if let Some(errno) = error.errno {
            return io::Error::from_raw_os_error(errno);
        }
        io::Error::new(
            match error.kind {
                IoFailureKind::TimedOut => io::ErrorKind::TimedOut,
                IoFailureKind::InvalidData => io::ErrorKind::InvalidData,
                IoFailureKind::Other => io::ErrorKind::Other,
            },
            error.message,
        )
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    /// Omitted v1 policy still terminates on owner exit; unknown fields cannot change that policy.
    #[test]
    fn requests_validate_owner_policy_and_unknown_fields() {
        let request = serde_json::json!({
            "program": "/bin/true",
            "args": [],
            "timeout": Duration::from_secs(1),
        });
        let mut unknown = request.clone();
        unknown["unknown_policy"] = serde_json::json!(true);
        check_values(
            [
                Check {
                    scenario: "omitted policy retains cancellation",
                    input: request,
                    expect: Some(ToolOwnerExitBehavior::Terminate),
                },
                Check {
                    scenario: "unrecognized request field is rejected",
                    input: unknown,
                    expect: None,
                },
            ],
            |request| {
                serde_json::from_value::<Request>(request)
                    .map(|request| request.owner_exit_behavior)
                    .ok()
            },
        );
    }
}
