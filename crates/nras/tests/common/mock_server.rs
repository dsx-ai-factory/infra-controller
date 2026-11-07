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

pub(crate) enum Method {
    Get,
    Post,
}

impl Method {
    fn to_string(&self) -> &str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

pub(crate) fn add_mock(
    server: &mut mockito::ServerGuard,
    path: &str,
    response_body: &str,
    method: &Method,
    status_code: usize,
) -> String {
    // Create a mock
    server
        .mock(method.to_string(), path)
        .with_status(status_code)
        .with_header("content-type", "application/json")
        .with_body(response_body)
        .create();

    format!("{}{}", server.url(), path)
}

pub(crate) async fn create_mock_http_server() -> mockito::ServerGuard {
    // Request a new server from the pool
    mockito::Server::new_async().await
}
