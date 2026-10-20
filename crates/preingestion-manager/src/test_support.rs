// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Helpers for preingestion integration tests.

use std::sync::Arc;
use std::time::Duration;

use crate::PreingestionManager;

/// Override firmware submission and status request deadlines for one test manager.
pub fn set_rack_firmware_request_timeout(manager: &mut PreingestionManager, timeout: Duration) {
    let static_info = Arc::make_mut(&mut manager.static_info);
    static_info.rack_firmware_submission_timeout = timeout;
    static_info.rack_firmware_status_timeout = timeout;
}
