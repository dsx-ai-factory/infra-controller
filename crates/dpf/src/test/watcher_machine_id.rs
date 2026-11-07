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

//! Tests for machine ID resolution in watcher events.

use std::sync::Arc;

use super::helpers::{Collector, WatcherMock, make_dpu, make_dpu_labeled};
use crate::crds::dpus_generated::*;
use crate::types::*;
use crate::watcher::DpuWatcherBuilder;

const TEST_NS: &str = "watcher-machine-id-ns";

#[tokio::test]
async fn test_machine_id_from_label() {
    let m = Arc::new(WatcherMock::new());
    let c = Arc::new(Collector::<DpuEvent>::default());
    let cc = c.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_event(move |e| {
            let cc = cc.clone();
            async move {
                cc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu_labeled(
        TEST_NS,
        "d1",
        "dev",
        "n1",
        DpuStatusPhase::Ready,
        "my-machine-id",
    ));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().device_name, "dev");
}

#[tokio::test]
async fn test_machine_id_fallback() {
    let m = Arc::new(WatcherMock::new());
    let c = Arc::new(Collector::<DpuEvent>::default());
    let cc = c.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_event(move |e| {
            let cc = cc.clone();
            async move {
                cc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(
        TEST_NS,
        "d1",
        "fallback-dev",
        "n1",
        DpuStatusPhase::Ready,
    ));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().device_name, "fallback-dev");
}
