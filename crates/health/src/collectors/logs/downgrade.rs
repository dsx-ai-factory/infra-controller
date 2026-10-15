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

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Instant;

use carbide_uuid::rack::RackId;
use dashmap::DashMap;
use nv_redfish::core::ODataId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DowngradeReason {
    SseNotAvailable,
    ConnectFailureBudgetExhausted,
}

impl DowngradeReason {
    fn as_label(self) -> &'static str {
        match self {
            Self::SseNotAvailable => "sse_not_available",
            Self::ConnectFailureBudgetExhausted => "connect_failure_budget",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DowngradeEvent {
    /// Reason SSE collection stopped retrying.
    pub reason: DowngradeReason,

    /// Time at which the downgrade was recorded.
    pub at: Instant,
}

#[derive(Debug)]
struct DowngradeState {
    event: DowngradeEvent,
    pending_last_seen_ids: Option<HashMap<ODataId, i32>>,
}

#[derive(Debug, Default)]
pub struct LogDowngradeRegistry {
    downgraded: DashMap<Cow<'static, str>, DowngradeState>,
}

impl LogDowngradeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_downgraded(&self, key: &str) -> bool {
        self.downgraded.get(key).map(|state| state.event).is_some()
    }

    /// Records the first downgrade for `key` and emits one warning.
    ///
    /// The warning includes `rack_id` when the endpoint has a rack identity.
    /// `last_seen_ids` becomes the next periodic collector's startup cursor.
    /// Later calls for the same `key` do not change the recorded downgrade.
    pub fn mark_downgraded(
        &self,
        key: Cow<'static, str>,
        rack_id: Option<&RackId>,
        reason: DowngradeReason,
        last_seen_ids: HashMap<ODataId, i32>,
    ) {
        use dashmap::Entry;
        match self.downgraded.entry(key.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(DowngradeState {
                    event: DowngradeEvent {
                        reason,
                        at: Instant::now(),
                    },
                    pending_last_seen_ids: Some(last_seen_ids),
                });
                tracing::warn!(
                    endpoint_key = %key,
                    rack_id = rack_id.map(tracing::field::display),
                    reason = reason.as_label(),
                    "SSE log collector downgraded to periodic polling"
                );
            }
            Entry::Occupied(_) => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.downgraded.len()
    }

    /// Returns the pending SSE cursor without consuming it.
    pub(crate) fn pending_last_seen_ids(&self, key: &str) -> Option<HashMap<ODataId, i32>> {
        self.downgraded
            .get(key)
            .and_then(|state| state.pending_last_seen_ids.clone())
    }

    /// Takes the SSE cursor for the next periodic collector without clearing
    /// the endpoint's downgraded status.
    pub(crate) fn take_last_seen_ids(&self, key: &str) -> Option<HashMap<ODataId, i32>> {
        self.downgraded
            .get_mut(key)
            .and_then(|mut state| state.pending_last_seen_ids.take())
    }

    /// Clears the downgrade after SSE recovery or collector shutdown.
    pub(crate) fn clear_downgraded(&self, key: &str) -> bool {
        self.downgraded.remove(key).is_some()
    }

    /// Returns the recorded downgrade for an endpoint.
    #[cfg(test)]
    pub(crate) fn event_for(&self, key: &str) -> Option<DowngradeEvent> {
        self.downgraded.get(key).map(|entry| entry.event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fresh_registry_has_no_downgrades() {
        let registry = LogDowngradeRegistry::new();
        assert!(!registry.is_downgraded("any-key"));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_mark_downgraded_records_key_and_reason() {
        let registry = LogDowngradeRegistry::new();

        let last_seen_ids =
            HashMap::from([(ODataId::from("/redfish/v1/LogServices/1".to_string()), 42)]);

        registry.mark_downgraded(
            Cow::Borrowed("bmc-1"),
            None,
            DowngradeReason::SseNotAvailable,
            last_seen_ids.clone(),
        );

        assert!(registry.is_downgraded("bmc-1"));
        assert_eq!(registry.len(), 1);
        let event = registry
            .event_for("bmc-1")
            .expect("event should be recorded");

        assert_eq!(event.reason, DowngradeReason::SseNotAvailable);

        assert_eq!(
            registry.pending_last_seen_ids("bmc-1"),
            Some(last_seen_ids.clone())
        );

        assert_eq!(registry.take_last_seen_ids("bmc-1"), Some(last_seen_ids));
        assert!(registry.is_downgraded("bmc-1"));
        assert_eq!(registry.pending_last_seen_ids("bmc-1"), None);
    }

    #[test]
    fn test_mark_downgraded_is_idempotent_for_same_key() {
        let registry = LogDowngradeRegistry::new();

        registry.mark_downgraded(
            Cow::Borrowed("bmc-1"),
            None,
            DowngradeReason::SseNotAvailable,
            HashMap::new(),
        );

        let first = registry
            .event_for("bmc-1")
            .expect("first mark should record");

        // second mark is a no-op; original reason and timestamp stick
        std::thread::sleep(std::time::Duration::from_millis(2));
        registry.mark_downgraded(
            Cow::Borrowed("bmc-1"),
            None,
            DowngradeReason::ConnectFailureBudgetExhausted,
            HashMap::from([(ODataId::from("/redfish/v1/LogServices/1".to_string()), 99)]),
        );
        let second = registry
            .event_for("bmc-1")
            .expect("second mark should not clear the entry");

        assert_eq!(registry.len(), 1);
        assert_eq!(second.reason, DowngradeReason::SseNotAvailable);
        assert_eq!(second.at, first.at);
        assert_eq!(registry.take_last_seen_ids("bmc-1"), Some(HashMap::new()));
    }

    #[test]
    fn test_mark_downgraded_tracks_multiple_endpoints_independently() {
        let registry = LogDowngradeRegistry::new();

        registry.mark_downgraded(
            Cow::Borrowed("bmc-1"),
            None,
            DowngradeReason::SseNotAvailable,
            HashMap::new(),
        );

        registry.mark_downgraded(
            Cow::Borrowed("bmc-2"),
            None,
            DowngradeReason::ConnectFailureBudgetExhausted,
            HashMap::new(),
        );

        assert!(registry.is_downgraded("bmc-1"));
        assert!(registry.is_downgraded("bmc-2"));
        assert!(!registry.is_downgraded("bmc-3"));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn clear_downgraded_allows_a_fresh_cursor_handoff() {
        let registry = LogDowngradeRegistry::new();

        registry.mark_downgraded(
            Cow::Borrowed("bmc-1"),
            None,
            DowngradeReason::SseNotAvailable,
            HashMap::new(),
        );

        assert!(registry.clear_downgraded("bmc-1"));

        let last_seen_ids =
            HashMap::from([(ODataId::from("/redfish/v1/LogServices/1".to_string()), 42)]);

        registry.mark_downgraded(
            Cow::Borrowed("bmc-1"),
            None,
            DowngradeReason::ConnectFailureBudgetExhausted,
            last_seen_ids.clone(),
        );

        assert_eq!(registry.take_last_seen_ids("bmc-1"), Some(last_seen_ids));
    }
}
