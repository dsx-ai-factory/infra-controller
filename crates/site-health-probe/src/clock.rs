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

//! The wall clock of the state readers, the snapshot store and the API.
//! Tests run on tokio's paused clock, so there the wall clock follows tokio
//! time from the moment the test thread first read it; sweep times, ages and
//! the scrapes of a fake metrics store then share one timeline.

use std::time::SystemTime;

#[cfg(not(test))]
pub(crate) fn now() -> SystemTime {
    SystemTime::now()
}

#[cfg(test)]
pub(crate) fn now() -> SystemTime {
    use std::cell::Cell;

    thread_local! {
        static ORIGIN: Cell<Option<(SystemTime, tokio::time::Instant)>> = const { Cell::new(None) };
    }
    let (wall, tokio_at) = ORIGIN.with(|origin| match origin.get() {
        Some(pair) => pair,
        None => {
            let pair = (SystemTime::now(), tokio::time::Instant::now());
            origin.set(Some(pair));
            pair
        }
    });
    wall + tokio::time::Instant::now().saturating_duration_since(tokio_at)
}
