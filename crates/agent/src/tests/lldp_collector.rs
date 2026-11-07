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

use carbide_host_support::lldp_collector::LldpCollectorError;

use crate::{AgentPlatformType, collect_lldp_neighbors_with_collectors, run_lldp_sidecar_loop};

#[tokio::test]
async fn containerized_lldp_collection_preserves_snapshot_errors() {
    let result = collect_lldp_neighbors_with_collectors(
        &AgentPlatformType::Containerized,
        || std::future::ready(Ok(Vec::new())),
        || Err(LldpCollectorError::Lldp("snapshot unavailable".into())),
    )
    .await;

    assert!(matches!(
        result,
        Err(LldpCollectorError::Lldp(message)) if message == "snapshot unavailable"
    ));
}

#[tokio::test(start_paused = true)]
async fn sidecar_retries_past_three_failures_and_recovers() {
    let outcomes = std::sync::Mutex::new(std::collections::VecDeque::from([
        Err(LldpCollectorError::Lldp("first failure".into())),
        Err(LldpCollectorError::Lldp("second failure".into())),
        Err(LldpCollectorError::Lldp("third failure".into())),
        Ok(()),
    ]));

    run_lldp_sidecar_loop(
        || {
            std::future::ready(
                outcomes
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("configured outcome"),
            )
        },
        Some(4),
    )
    .await;

    assert!(outcomes.lock().unwrap().is_empty());
}
