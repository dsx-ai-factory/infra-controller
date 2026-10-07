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

use crate::redfish;

/// The serial-console payload exposed by OpenBMC bmcweb when its console
/// services are enabled. Hardware profiles must opt in only when their
/// captured Redfish data confirms this support.
pub(super) fn enabled_serial_console() -> redfish::serial_console::SerialConsole {
    redfish::serial_console::builder()
        .max_concurrent_sessions(15)
        .ssh(
            &redfish::serial_console::protocol_builder()
                .service_enabled(true)
                .port(2200)
                .hot_key_sequence_display("Press ~. to exit console")
                .build(),
        )
        .ipmi(
            &redfish::serial_console::protocol_builder()
                .service_enabled(true)
                .build(),
        )
        .build()
}
