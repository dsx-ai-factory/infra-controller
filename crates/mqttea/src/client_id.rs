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

//! Helpers for constructing MQTT broker-level client identifiers.
//!
//! MQTT brokers use the client identifier in the CONNECT packet to detect and
//! disconnect duplicate sessions. When two processes connect with the same
//! client identifier (for example two replicas of the same Deployment, or a
//! new pod coming up while the old one is still terminating), the broker
//! disconnects the older session and the loser auto-reconnects, producing a
//! ping-pong loop that looks like an outage even though OAuth2 authentication
//! and authorization are both healthy.
//!
//! [`unique_client_id`] returns `<base>-<8-hex-suffix>` where the suffix is
//! drawn from a fresh v4 UUID. Each process therefore picks a stable client
//! identifier for its lifetime, but two processes are vanishingly unlikely to
//! collide on the broker.

use uuid::Uuid;

/// Length of the random suffix appended to the base client identifier.
///
/// 8 hex characters give 32 bits of entropy, enough to avoid collisions across
/// the small handful of replicas a single MQTT-using deployment runs in
/// practice while keeping the resulting identifier short enough to read in
/// broker logs.
const SUFFIX_LEN: usize = 8;

/// Return `<base>-<random-suffix>` so each process picks a unique broker-level
/// client identifier.
///
/// # Example
///
/// ```
/// use mqttea::client_id::unique_client_id;
///
/// let id = unique_client_id("carbide-dsx-exchange-event-bus");
/// assert!(id.starts_with("carbide-dsx-exchange-event-bus-"));
/// assert_eq!(id.len(), "carbide-dsx-exchange-event-bus-".len() + 8);
/// ```
pub fn unique_client_id(base: &str) -> String {
    let suffix: String = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(SUFFIX_LEN)
        .collect();
    format!("{base}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_base_and_appends_suffix() {
        let id = unique_client_id("base");
        assert!(id.starts_with("base-"), "got {id}");
        assert_eq!(id.len(), "base-".len() + SUFFIX_LEN);
    }

    #[test]
    fn empty_base_still_produces_unique_suffix() {
        let id = unique_client_id("");
        assert!(id.starts_with('-'));
        assert_eq!(id.len(), 1 + SUFFIX_LEN);
    }

    #[test]
    fn suffix_is_hex() {
        let id = unique_client_id("base");
        let suffix = &id["base-".len()..];
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "expected hex suffix, got {suffix}"
        );
    }

    #[test]
    fn two_calls_almost_never_collide() {
        let a = unique_client_id("base");
        let b = unique_client_id("base");
        assert_ne!(a, b);
    }
}
