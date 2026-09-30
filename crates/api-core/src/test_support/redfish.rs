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

pub mod test_behavior {
    pub fn success() -> serde_json::Value {
        crate::handlers::redfish::test_behavior::success()
    }

    pub fn failure_at_request() -> serde_json::Value {
        crate::handlers::redfish::test_behavior::failure_at_request()
    }

    pub fn failure_at_client_creation() -> serde_json::Value {
        crate::handlers::redfish::test_behavior::failure_at_client_creation()
    }

    pub fn request_failure_description() -> String {
        crate::handlers::redfish::test_behavior::request_failure_description()
    }
}
