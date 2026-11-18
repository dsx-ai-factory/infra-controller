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

//! How driver and catalogue errors end a check: unsupported, unreachable,
//! not ready or a server error (which the runner retries), or failed.

use bmc_drivers::CatalogError;
use bmc_platform::PlatformError;

use super::model::Outcome;

impl From<CatalogError> for Outcome {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::Unsupported { .. } => Self::Unsupported(error.to_string()),
            other => Self::Fail(other.to_string()),
        }
    }
}

impl From<PlatformError> for Outcome {
    fn from(error: PlatformError) -> Self {
        let reason = shortened(error.to_string());
        ended(error, reason)
    }
}

/// [`Outcome::from`], naming `operation` for a step that makes several
/// driver calls.
pub(super) fn failed(operation: &'static str) -> impl FnOnce(PlatformError) -> Outcome {
    move |error| {
        let reason = describe(operation, &error);
        ended(error, reason)
    }
}

/// `operation`'s error, cut short: a BMC can answer with a whole error page.
pub(super) fn describe(operation: &str, error: &PlatformError) -> String {
    format!("{operation}: {}", shortened(error.to_string()))
}

fn ended(error: PlatformError, reason: String) -> Outcome {
    match error {
        PlatformError::Unsupported => Outcome::Unsupported(reason),
        // A server error is the BMC failing to handle the request, which
        // asking again can get past; 501 says it never will.
        PlatformError::Unreachable
        | PlatformError::NotReady { .. }
        | PlatformError::Bmc {
            status: 500 | 502 | 503 | 504,
            ..
        } => Outcome::Unreachable(reason),
        _ => Outcome::Fail(reason),
    }
}

fn shortened(text: String) -> String {
    const MAX_CHARS: usize = 400;
    match text.char_indices().nth(MAX_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BMC that is not ready yet or fails with a server error is retried
    /// like one that did not answer; a request it rejects or will never
    /// implement fails, and only an unsupported answer skips the check.
    #[test]
    fn errors_that_asking_again_can_clear_are_retried() {
        let bmc = |status| PlatformError::Bmc {
            status,
            message_id: None,
            message: "refused".to_string(),
        };
        let not_ready = PlatformError::NotReady {
            message: "the BIOS has not reported its attributes".to_string(),
        };
        for (scenario, error, retried) in [
            ("not ready", not_ready, true),
            ("internal server error", bmc(500), true),
            ("service unavailable", bmc(503), true),
            ("rejected request", bmc(400), false),
            ("not implemented", bmc(501), false),
        ] {
            assert_eq!(
                matches!(Outcome::from(error), Outcome::Unreachable(_)),
                retried,
                "{scenario}"
            );
        }
        assert!(matches!(
            Outcome::from(PlatformError::Unsupported),
            Outcome::Unsupported(_)
        ));
    }
}
