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

//! SLAs for Dpa Interface State Machine Controller

use std::time::Duration;

// TODO(chet): Revisit these SLAs -- they seem a little high. Operations
// like lock/unlock are pretty instantaneous, and profile is ~seconds.
pub(super) const LOCKING: Duration = Duration::from_secs(15 * 60);
// ...BUT applying firmware actually can take a while. SuperNIC flashing
// seems to be roughly 7 minutes to flash then 1 minute to reset, but
// that's resetting the device, not the entire host. As of now it seems
// like resetting the device is enough, but we may end up needing to
// do a full power cycle of the host, which would definitely take a bit.
pub(super) const APPLY_FIRMWARE: Duration = Duration::from_secs(30 * 60);
pub(super) const APPLY_PROFILE: Duration = Duration::from_secs(15 * 60);
pub(super) const UNLOCKING: Duration = Duration::from_secs(15 * 60);
// Tenant-free NIC lockdown IKM rekey sub-states. Lock/unlock are as
// instantaneous as their assignment-cycle counterparts, so mirror UNLOCKING.
pub(super) const ROTATE_KEY_UNLOCKING: Duration = Duration::from_secs(15 * 60);
pub(super) const ROTATE_KEY_LOCKING: Duration = Duration::from_secs(15 * 60);
