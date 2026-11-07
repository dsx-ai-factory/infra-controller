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

//! Tests for the on_dpu_event callback.

use std::sync::Arc;

use super::helpers::{Collector, WatcherMock, make_dpu};
use crate::crds::dpus_generated::*;
use crate::types::*;
use crate::watcher::DpuWatcherBuilder;

const TEST_NS: &str = "watcher-phase-ns";

#[tokio::test]
async fn test_dpu_event_ready() {
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
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev", "n1", DpuStatusPhase::Ready));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().phase, DpuPhase::Ready);
}

#[tokio::test]
async fn test_dpu_event_error() {
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
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev", "n1", DpuStatusPhase::Error));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().phase, DpuPhase::Error);
}

#[tokio::test]
async fn test_dpu_event_provisioning() {
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
        "dev",
        "n1",
        DpuStatusPhase::Initializing,
    ));
    m.emit_dpu(make_dpu(
        TEST_NS,
        "d2",
        "dev",
        "n1",
        DpuStatusPhase::Pending,
    ));
    m.emit_dpu(make_dpu(
        TEST_NS,
        "d3",
        "dev",
        "n1",
        DpuStatusPhase::OsInstalling,
    ));
    c.wait_for(3).await;
    let phases: Vec<_> = c.all().iter().map(|e| e.phase.clone()).collect();
    assert!(phases.contains(&DpuPhase::Provisioning("Initializing".into())));
    assert!(phases.contains(&DpuPhase::Provisioning("Pending".into())));
    assert!(phases.contains(&DpuPhase::Provisioning("OsInstalling".into())));
}

#[tokio::test]
async fn test_dpu_event_deleting() {
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
        "dev",
        "n1",
        DpuStatusPhase::Deleting,
    ));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().phase, DpuPhase::Deleting);
}

#[tokio::test]
async fn test_dpu_event_rebooting() {
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
        "dev",
        "n1",
        DpuStatusPhase::Rebooting,
    ));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().phase, DpuPhase::Rebooting);
}

#[tokio::test]
async fn test_dpu_event_node_effect() {
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
        "dev",
        "n1",
        DpuStatusPhase::NodeEffect,
    ));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().phase, DpuPhase::NodeEffect);
}
