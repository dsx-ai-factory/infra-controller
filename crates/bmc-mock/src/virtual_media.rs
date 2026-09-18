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

/// Desired contents of one configured virtual media device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualMediaState {
    /// ID of the configured Redfish VirtualMedia device.
    pub device_id: String,
    /// Image path or URI, or None when the device is empty.
    pub image: Option<String>,
    /// Whether the inserted image is read-only.
    pub write_protected: bool,
}

impl VirtualMediaState {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "Id": self.device_id,
            "Image": self.image,
            "Inserted": self.image.is_some(),
            "WriteProtected": self.write_protected,
        })
    }
}
