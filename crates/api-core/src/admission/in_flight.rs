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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use carbide_authn::middleware::ExternalUserInfo;

use super::engine::ClientKeyRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, carbide_instrument::LabelValue)]
pub(super) enum ClientKindLabel {
    ExternalUser,
    Service,
    Machine,
    Default,
}

impl From<ClientKeyRef<'_>> for ClientKindLabel {
    fn from(key: ClientKeyRef<'_>) -> Self {
        match key {
            ClientKeyRef::ExternalUser(_) => Self::ExternalUser,
            ClientKeyRef::ServiceId(_) => Self::Service,
            ClientKeyRef::MachineId(_) => Self::Machine,
            ClientKeyRef::Default => Self::Default,
        }
    }
}

pub(super) fn client_identifier(key: ClientKeyRef<'_>) -> String {
    match key {
        ClientKeyRef::ExternalUser(ExternalUserInfo { group, user, .. }) => {
            if let Some(user) = user {
                format!("{group}/{user}")
            } else {
                group.clone()
            }
        }
        ClientKeyRef::ServiceId(id) | ClientKeyRef::MachineId(id) => id.to_owned(),
        ClientKeyRef::Default => "default".to_owned(),
    }
}

/// Parses a gRPC `grpc-timeout` header value. Returns `None` when malformed.
pub(super) fn parse_grpc_timeout(header: &str) -> Option<Duration> {
    if header.is_empty() {
        return None;
    }
    let (amount, unit) = header.split_at(header.len().checked_sub(1)?);
    let amount: u64 = amount.parse().ok()?;
    match unit {
        "H" => Some(Duration::from_secs(amount.saturating_mul(60 * 60))),
        "M" => Some(Duration::from_secs(amount.saturating_mul(60))),
        "S" => Some(Duration::from_secs(amount)),
        "m" => Some(Duration::from_millis(amount)),
        "u" => Some(Duration::from_micros(amount)),
        "n" => Some(Duration::from_nanos(amount)),
        _ => None,
    }
}

struct InFlightEntry {
    route: String,
    client_kind: ClientKindLabel,
    client_identifier: String,
    started: Instant,
    deadline: Duration,
}

pub(super) struct ExecutionRegistry {
    next_id: AtomicU64,
    entries: Mutex<HashMap<u64, InFlightEntry>>,
}

impl ExecutionRegistry {
    pub(super) fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Records an admitted request until the returned guard is dropped. The
    /// guard, not the caller, removes the entry, so a request whose future is
    /// dropped mid-handler (a client cancelling with `RST_STREAM`) is removed
    /// too.
    pub(super) fn register(
        self: &Arc<Self>,
        route: String,
        client_kind: ClientKindLabel,
        client_identifier: String,
        deadline: Duration,
    ) -> InFlightGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock_entries().insert(
            id,
            InFlightEntry {
                route,
                client_kind,
                client_identifier,
                started: Instant::now(),
                deadline,
            },
        );
        InFlightGuard {
            registry: Arc::clone(self),
            id,
        }
    }

    fn remove(&self, id: u64) {
        self.lock_entries().remove(&id);
    }

    pub(super) fn oldest_execution_age(&self) -> Duration {
        let now = Instant::now();
        self.lock_entries()
            .values()
            .map(|entry| now.saturating_duration_since(entry.started))
            .max()
            .unwrap_or(Duration::ZERO)
    }

    pub(super) fn work_in_flight_by_client_kind(&self) -> HashMap<ClientKindLabel, u64> {
        let mut counts = HashMap::new();
        for entry in self.lock_entries().values() {
            *counts.entry(entry.client_kind).or_insert(0) += 1;
        }
        counts
    }

    pub(super) fn entries_older_than_half_deadline(&self) -> Vec<(String, String, Duration)> {
        let now = Instant::now();
        self.lock_entries()
            .values()
            .filter_map(|entry| {
                let elapsed = now.saturating_duration_since(entry.started);
                let threshold = entry.deadline.checked_div(2)?;
                (elapsed >= threshold).then(|| {
                    (
                        entry.route.clone(),
                        entry.client_identifier.clone(),
                        elapsed,
                    )
                })
            })
            .collect()
    }

    fn lock_entries(&self) -> MutexGuard<'_, HashMap<u64, InFlightEntry>> {
        self.entries
            .lock()
            .expect("execution registry mutex must not be poisoned")
    }
}

/// Removes its request from the [`ExecutionRegistry`] when dropped.
pub(super) struct InFlightGuard {
    registry: Arc<ExecutionRegistry>,
    id: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.registry.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_timeout_parsing() {
        let cases = [
            ("1S", Some(Duration::from_secs(1))),
            ("500m", Some(Duration::from_millis(500))),
            ("2M", Some(Duration::from_secs(120))),
            ("", None),
            ("not-a-timeout", None),
            ("1X", None),
        ];
        for (header, expected) in cases {
            assert_eq!(parse_grpc_timeout(header), expected, "header: {header}");
        }
    }
}
