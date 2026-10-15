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

//! Background refresh of the desired firmware versions shared through `MachineATronContext`.

use std::sync::{Arc, RwLock};

use rpc::forge::DesiredFirmwareVersionEntry;

use crate::MachineATronContext;

/// Spawn the background task that re-fetches desired firmware versions every
/// `api_refresh_interval`.
pub fn spawn_desired_firmware_refresher(app_context: Arc<MachineATronContext>) {
    tokio::task::Builder::new()
        .name("DesiredFirmwareRefresher")
        .spawn(async move {
            let mut interval = tokio::time::interval(app_context.app_config.api_refresh_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await; // the startup fetch already populated the context
            loop {
                interval.tick().await;
                let fetched = app_context
                    .forge_api_client
                    .get_desired_firmware_versions()
                    .await
                    .map(|response| response.entries);
                refresh_desired_firmware_versions(&app_context.desired_firmware_versions, fetched);
            }
        })
        .unwrap();
}

/// Replace the shared targets with a successful fetch, including an empty one;
/// a failed fetch keeps the last known targets.
fn refresh_desired_firmware_versions(
    current: &RwLock<Vec<DesiredFirmwareVersionEntry>>,
    fetched: Result<Vec<DesiredFirmwareVersionEntry>, tonic::Status>,
) {
    let entries = match fetched {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                %error,
                "Failed to refresh desired firmware versions; keeping last known",
            );
            return;
        }
    };
    let mut current = current.write().unwrap();
    if *current != entries {
        tracing::info!(
            desired_firmware_versions = ?entries,
            "Desired firmware versions changed",
        );
        *current = entries;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn desired_firmware_refresh_follows_the_api_except_on_failure() {
        fn entry(bmc: &str) -> DesiredFirmwareVersionEntry {
            DesiredFirmwareVersionEntry {
                vendor: "Dell".to_string(),
                model: "PowerEdge R750".to_string(),
                component_versions: HashMap::from([("bmc".to_string(), bmc.to_string())]),
            }
        }
        let configured = vec![entry("7.10")];

        check_values(
            [
                Check {
                    scenario: "changed response replaces the targets",
                    input: Ok(vec![entry("7.20")]),
                    expect: vec![entry("7.20")],
                },
                Check {
                    scenario: "empty response clears the targets",
                    input: Ok(vec![]),
                    expect: vec![],
                },
                Check {
                    scenario: "failed fetch keeps the last known targets",
                    input: Err(tonic::Status::unavailable("api unreachable")),
                    expect: configured.clone(),
                },
            ],
            |fetched| {
                let current = RwLock::new(configured.clone());
                refresh_desired_firmware_versions(&current, fetched);
                current.into_inner().unwrap()
            },
        );
    }
}
