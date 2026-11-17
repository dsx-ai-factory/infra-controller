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

//! Response details shared by the UFM, NMX-C, and Redfish browser pages,
//! rendered by the `common_ui.html` macros.

/// One row of a browser page's response headers table.
pub(super) struct ResponseHeader {
    pub(super) name: String,
    pub(super) value: String,
}

pub(super) fn response_headers(
    headers: impl IntoIterator<Item = (String, String)>,
) -> Vec<ResponseHeader> {
    headers
        .into_iter()
        .map(|(name, value)| ResponseHeader { name, value })
        .collect()
}

/// Reason phrase for an HTTP status code, e.g. "Not Found"; empty when unknown.
pub(super) fn status_reason(code: u16) -> String {
    http::StatusCode::from_u16(code)
        .map(|code| code.canonical_reason().unwrap_or_default().to_string())
        .unwrap_or_default()
}
