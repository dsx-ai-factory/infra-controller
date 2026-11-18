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

//! Soft stop and hard cancellation, shared by the runner, the writer and the
//! observer.

use tokio_util::sync::CancellationToken;

/// Soft stop and hard cancellation of a running test. A hard cancel also
/// counts as a stop request.
#[derive(Debug, Clone, Default)]
pub struct ControlWatch {
    stop: CancellationToken,
    cancel: CancellationToken,
}

impl ControlWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Finish what is in flight, send nothing new.
    pub fn request_stop(&self) {
        self.stop.cancel();
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.is_cancelled() || self.cancel.is_cancelled()
    }

    /// The lock is lost or the run must end now.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_watch_cancel_implies_stop() {
        let watch = ControlWatch::new();
        assert!(!watch.stop_requested());
        watch.request_stop();
        assert!(watch.stop_requested() && !watch.is_cancelled());
        let watch = ControlWatch::new();
        watch.cancel();
        assert!(watch.stop_requested() && watch.is_cancelled());
    }
}
