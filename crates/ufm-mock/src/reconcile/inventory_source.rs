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

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use url::Url;

use crate::config::InventorySourceConfig;
use crate::inventory::{InventoryId, InventoryLease, InventorySnapshot};

pub(super) struct PolledInventorySource {
    url: Url,
    client: Option<reqwest::Client>,
    client_error: Option<String>,
    known_inventory_id: Option<InventoryId>,
    lease: InventoryLease,
    failure_since: Option<Instant>,
    failure_applied: bool,
}

pub(super) enum PollOutcome {
    ClientUnavailable,
    Succeeded {
        snapshot: InventorySnapshot,
        previous_inventory_id: Option<InventoryId>,
    },
    Failed(String),
}

pub(super) enum SourceFailure<'a> {
    ClientConfiguration(String),
    Inventory {
        inventory_id: &'a InventoryId,
        lease: InventoryLease,
    },
}

pub(super) struct LogSource<'a>(pub(super) &'a PolledInventorySource);

impl fmt::Display for LogSource<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.url.as_str())
    }
}

impl PolledInventorySource {
    pub(super) fn new(
        source: InventorySourceConfig,
        timeout: Duration,
        lease: InventoryLease,
    ) -> Self {
        match build_client(&source, timeout) {
            Ok(client) => Self {
                url: source.url,
                client: Some(client),
                client_error: None,
                known_inventory_id: None,
                lease,
                failure_since: None,
                failure_applied: false,
            },
            Err(error) => Self {
                url: source.url,
                client: None,
                client_error: Some(error.to_string()),
                known_inventory_id: None,
                lease,
                failure_since: Some(Instant::now()),
                failure_applied: false,
            },
        }
    }

    pub(super) fn lease(&self) -> InventoryLease {
        self.lease
    }

    pub(super) fn take_failure(
        &mut self,
        now: Instant,
        grace_period: Duration,
    ) -> Option<SourceFailure<'_>> {
        if self.failure_applied {
            return None;
        }
        if let Some(error) = self.client_error.take() {
            self.failure_applied = true;
            return Some(SourceFailure::ClientConfiguration(error));
        }
        if !self
            .failure_since
            .is_some_and(|start| now.duration_since(start) >= grace_period)
        {
            return None;
        }
        self.failure_applied = true;
        self.known_inventory_id
            .as_ref()
            .map(|inventory_id| SourceFailure::Inventory {
                inventory_id,
                lease: self.lease,
            })
    }

    pub(super) async fn poll(mut self, request_semaphore: Arc<Semaphore>) -> (Self, PollOutcome) {
        let Some(client) = self.client.as_ref() else {
            return (self, PollOutcome::ClientUnavailable);
        };
        let _permit = request_semaphore
            .acquire_owned()
            .await
            .expect("inventory request semaphore must remain open");
        let result = client
            .get(self.url.as_str())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status);
        let outcome = match result {
            Ok(response) => response
                .json::<InventorySnapshot>()
                .await
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        let outcome = match outcome {
            Ok(snapshot) => {
                let previous_inventory_id = self
                    .known_inventory_id
                    .replace(snapshot.inventory_id.clone())
                    .filter(|inventory_id| inventory_id != &snapshot.inventory_id);
                self.failure_since = None;
                self.failure_applied = false;
                PollOutcome::Succeeded {
                    snapshot,
                    previous_inventory_id,
                }
            }
            Err(error) => {
                self.failure_since.get_or_insert_with(Instant::now);
                PollOutcome::Failed(error)
            }
        };
        (self, outcome)
    }
}

fn build_client(
    source: &InventorySourceConfig,
    timeout: Duration,
) -> eyre::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .danger_accept_invalid_certs(source.insecure_skip_verify);
    if let Some(path) = source.ca_cert_path.as_ref() {
        let certificate = reqwest::Certificate::from_pem(&std::fs::read(path)?)?;
        builder = builder.add_root_certificate(certificate);
    }
    Ok(builder.build()?)
}
