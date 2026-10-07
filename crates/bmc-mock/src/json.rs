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

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;

pub(super) trait JsonExt {
    fn patch(self, patch: impl JsonPatch) -> serde_json::Value
    where
        Self: Sized;

    fn delete_fields(self, fields: &[&str]) -> serde_json::Value
    where
        Self: Sized;

    fn into_ok_response(self) -> Response<Body>
    where
        Self: Sized + ToString,
    {
        self.into_response(StatusCode::OK)
    }

    fn into_response(self, status: StatusCode) -> Response<Body>
    where
        Self: Sized + ToString;

    fn into_ok_response_with_location(self, location: HeaderValue) -> Response<Body>
    where
        Self: Sized + ToString,
    {
        let mut response = self.into_ok_response();
        response.headers_mut().insert("Location", location);
        response
    }
}

impl JsonExt for serde_json::Value {
    fn patch(mut self, patch: impl JsonPatch) -> serde_json::Value {
        json_patch(&mut self, patch.json_patch());
        self
    }

    fn delete_fields(mut self, fields: &[&str]) -> serde_json::Value {
        if let serde_json::Value::Object(obj) = &mut self {
            for f in fields {
                obj.remove(*f);
            }
        }
        self
    }

    fn into_response(self, status: StatusCode) -> Response<Body>
    where
        Self: Sized + ToString,
    {
        Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .body(Body::from(self.to_string()))
            .unwrap()
    }
}

pub(super) trait JsonPatch {
    fn json_patch(&self) -> serde_json::Value;
}

impl JsonPatch for serde_json::Value {
    fn json_patch(&self) -> serde_json::Value {
        self.clone()
    }
}

pub(super) fn json_patch(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target_obj), serde_json::Value::Object(patch_obj)) => {
            for (k, v_patch) in patch_obj {
                match target_obj.get_mut(&k) {
                    Some(v_target) => json_patch(v_target, v_patch),
                    None => {
                        target_obj.insert(k, v_patch);
                    }
                }
            }
        }
        (target_slot, v_patch) => *target_slot = v_patch,
    }
}
