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

use std::borrow::Cow;

use serde_json::json;

use crate::json::{JsonExt, JsonPatch};

/// Defines minimal set of Redfish resource attributes.
pub(crate) struct Collection<'a> {
    pub(crate) odata_id: Cow<'a, str>,
    pub(crate) odata_type: Cow<'a, str>,
    pub(crate) name: Cow<'a, str>,
}

impl Collection<'_> {
    pub(crate) fn nav_property(&self, name: &str) -> serde_json::Value {
        json!({
            name: {
                "@odata.id": self.odata_id
            }
        })
    }

    pub(crate) fn with_members(&self, members: &[impl serde::Serialize]) -> serde_json::Value {
        let count = members.len();
        self.json_patch().patch(json!({
            "Members": members,
            "Members@odata.count": count,
        }))
    }
}

impl<'a> AsRef<Collection<'a>> for Collection<'a> {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl JsonPatch for Collection<'_> {
    fn json_patch(&self) -> serde_json::Value {
        json!({
            "@odata.id": self.odata_id,
            "@odata.type": self.odata_type,
            "Name": self.name,
        })
    }
}
