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

use std::ops::Deref;

use nv_redfish::core::{EntityTypeRef, ODataETag, ODataId};
use serde::Deserialize;

/// A resource fetched by id whose body has no generated schema type.
///
/// Wraps a payload struct with the OData envelope so it satisfies the
/// `EntityTypeRef` bound of `Bmc::get` and can be patched back with its ETag.
#[derive(Debug, Deserialize)]
pub struct Fetched<T> {
    #[serde(rename = "@odata.id", default = "unknown_id")]
    odata_id: ODataId,
    #[serde(rename = "@odata.etag", default)]
    etag: Option<ODataETag>,
    #[serde(flatten)]
    body: T,
}

fn unknown_id() -> ODataId {
    ODataId::from(String::new())
}

impl<T> Fetched<T> {
    /// Consumes the envelope, returning the payload.
    pub fn into_body(self) -> T {
        self.body
    }
}

impl<T> Deref for Fetched<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.body
    }
}

impl<T: Send + Sync> EntityTypeRef for Fetched<T> {
    fn odata_id(&self) -> &ODataId {
        &self.odata_id
    }

    fn etag(&self) -> Option<&ODataETag> {
        self.etag.as_ref()
    }
}
