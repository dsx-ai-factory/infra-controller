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

use serde_json::json;

use crate::json::JsonExt;
use crate::redfish::ModelExt;

#[derive(Clone)]
pub(crate) struct SerialConsole {
    value: serde_json::Value,
}

impl SerialConsole {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }

    pub(crate) fn has_enabled_ssh(&self) -> bool {
        self.value
            .get("SSH")
            .and_then(|ssh| ssh.get("ServiceEnabled"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    }

    pub(crate) fn with_ssh_port(&self, port: u16) -> Self {
        Self {
            value: self.value.clone().patch(json!({ "SSH": { "Port": port } })),
        }
    }
}

pub(crate) fn builder() -> super::schema::computer_system::HostSerialConsole {
    super::schema::computer_system::HostSerialConsole::new()
}

pub(crate) fn protocol_builder() -> super::schema::computer_system::SerialConsoleProtocol {
    super::schema::computer_system::SerialConsoleProtocol::new()
}

pub(crate) fn simulated_ssh(port: u16) -> SerialConsole {
    builder()
        .ssh(
            protocol_builder()
                .service_enabled(true)
                .port(i64::from(port)),
        )
        // Legacy libredfish requires both protocols whenever SerialConsole is present.
        .ipmi(protocol_builder().service_enabled(false))
        .build()
}

impl super::schema::computer_system::HostSerialConsole {
    pub(crate) fn build(self) -> SerialConsole {
        SerialConsole {
            value: self.into_json(),
        }
    }
}
