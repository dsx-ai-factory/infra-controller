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

use std::sync::Arc;

use crate::MachineATronContext;

/// Spawn the background task that re-fetches desired firmware versions every
/// `api_refresh_interval`. Empty responses and fetch errors keep the last known targets.
pub fn spawn_desired_firmware_refresher(app_context: Arc<MachineATronContext>) {
    tokio::task::Builder::new()
        .name("DesiredFirmwareRefresher")
        .spawn(async move {
            let mut interval = tokio::time::interval(app_context.app_config.api_refresh_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await; // the startup fetch already populated the context
            loop {
                interval.tick().await;
                match app_context
                    .forge_api_client
                    .get_desired_firmware_versions()
                    .await
                {
                    Ok(response) => {
                        if response.entries.is_empty() {
                            continue;
                        }
                        let mut current = app_context.desired_firmware_versions.write().unwrap();
                        if *current != response.entries {
                            tracing::info!(
                                desired_firmware_versions = ?response.entries,
                                "Desired firmware versions changed",
                            );
                            *current = response.entries;
                        }
                    }
                    Err(error) => tracing::warn!(
                        %error,
                        "Failed to refresh desired firmware versions; keeping last known",
                    ),
                }
            }
        })
        .unwrap();
}
