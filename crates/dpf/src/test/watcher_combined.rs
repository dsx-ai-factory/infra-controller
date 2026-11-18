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

//! Tests for combined callback behavior and event filtering.

use std::sync::Arc;

use super::helpers::{Collector, WatcherMock, make_dpu, make_dpu_reboot};
use crate::crds::dpus_generated::*;
use crate::types::*;
use crate::watcher::DpuWatcherBuilder;

const TEST_NS: &str = "watcher-combined-ns";

#[tokio::test]
async fn test_ready_and_dpu_event_both_fire() {
    let m = Arc::new(WatcherMock::new());
    let dpu_events = Arc::new(Collector::<DpuEvent>::default());
    let ready = Arc::new(Collector::<DpuReadyEvent>::default());
    let dc = dpu_events.clone();
    let rc = ready.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_event(move |e| {
            let dc = dc.clone();
            async move {
                dc.push(e);
                Ok(())
            }
        })
        .on_dpu_ready(move |e| {
            let rc = rc.clone();
            async move {
                rc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev", "n1", DpuStatusPhase::Ready));
    dpu_events.wait_for(1).await;
    ready.wait_for(1).await;
    assert_eq!(dpu_events.get(0).unwrap().phase, DpuPhase::Ready);
    assert_eq!(ready.get(0).unwrap().dpu_name, "d1");
}

#[tokio::test]
async fn test_dpu_event_and_reboot_both_fire() {
    let m = Arc::new(WatcherMock::new());
    let dpu_events = Arc::new(Collector::<DpuEvent>::default());
    let reboot = Arc::new(Collector::<RebootRequiredEvent>::default());
    let dc = dpu_events.clone();
    let rc = reboot.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_event(move |e| {
            let dc = dc.clone();
            async move {
                dc.push(e);
                Ok(())
            }
        })
        .on_reboot_required(move |e| {
            let rc = rc.clone();
            async move {
                rc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu_reboot(TEST_NS, "d1", "dev", "n1"));
    dpu_events.wait_for(1).await;
    reboot.wait_for(1).await;
    assert_eq!(dpu_events.get(0).unwrap().phase, DpuPhase::Rebooting);
    assert_eq!(reboot.get(0).unwrap().dpu_name, "d1");
}

#[tokio::test]
async fn test_dpu_no_status_ignored() {
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
    let mut dpu_no_status = make_dpu(TEST_NS, "d1", "dev", "n1", DpuStatusPhase::Ready);
    dpu_no_status.status = None;
    m.emit_dpu(dpu_no_status);
    let mut dpu_no_phase = make_dpu(TEST_NS, "d2", "dev", "n1", DpuStatusPhase::Ready);
    dpu_no_phase.status.as_mut().unwrap().phase = None;
    m.emit_dpu(dpu_no_phase);
    m.emit_dpu(make_dpu(
        TEST_NS,
        "sentinel",
        "dev",
        "n1",
        DpuStatusPhase::Ready,
    ));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().dpu_name, "sentinel");
}

#[tokio::test]
async fn test_multiple_events() {
    let m = Arc::new(WatcherMock::new());
    let dpu_events = Arc::new(Collector::<DpuEvent>::default());
    let ready = Arc::new(Collector::<DpuReadyEvent>::default());
    let dc = dpu_events.clone();
    let rc = ready.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_event(move |e| {
            let dc = dc.clone();
            async move {
                dc.push(e);
                Ok(())
            }
        })
        .on_dpu_ready(move |e| {
            let rc = rc.clone();
            async move {
                rc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    for i in 0..5 {
        let p = if i % 2 == 0 {
            DpuStatusPhase::Ready
        } else {
            DpuStatusPhase::Pending
        };
        m.emit_dpu(make_dpu(TEST_NS, &format!("d{}", i), "dev", "n1", p));
    }
    dpu_events.wait_for(5).await;
    ready.wait_for(3).await;
    assert_eq!(dpu_events.len(), 5);
    assert_eq!(ready.len(), 3);
}
