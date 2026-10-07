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
use std::str::FromStr;

use axum::http::uri::PathAndQuery;
use axum::http::{Request, Uri};
use axum::response::Response;

pub(super) mod logging;
pub(super) mod metrics;

pub(super) async fn normalize_url<B>(mut request: Request<B>) -> Request<B> {
    let uri = request.uri_mut();
    if let Some(p_q) = uri.path_and_query() {
        let path_and_query = p_q.to_string();
        let removed_duplicated_slashes = path_and_query.as_str().replace("//", "/");
        let new_path_and_query = match PathAndQuery::from_str(removed_duplicated_slashes.as_str()) {
            Ok(path_and_query) => path_and_query,
            _ => unreachable!(),
        };
        *uri = Uri::from(new_path_and_query);
    }
    request
}

// Converts content-length -> Content-Length to work with BMC http-download FW 24.07
pub(super) async fn fix_content_length_header<B>(mut response: Response<B>) -> Response<B> {
    if let Some(value) = response.headers_mut().remove("content-length") {
        response.headers_mut().insert("Content-Length", value);
    }

    response
}

#[cfg(test)]
mod test {
    use std::fs::File;
    use std::io::Write;

    use axum::Router;
    use axum::body::Body;
    use axum::http::StatusCode;
    use tempfile::TempDir;
    use tower::ServiceExt;
    use tower_http::services::ServeDir;

    use super::*;
    #[tokio::test]
    async fn test_url_normalize() {
        let request = Request::builder()
            .uri("http://localhost:8080/api/v0/cloud-init//dpu/user-data")
            .body(())
            .unwrap();
        let result = normalize_url(request).await;
        assert_eq!(result.uri().path(), "/api/v0/cloud-init/dpu/user-data");
    }

    #[tokio::test]
    async fn test_content_length_header() -> Result<(), Box<dyn std::error::Error>> {
        let tmp_dir = TempDir::new()?;
        let file_path = tmp_dir.path().join("file.txt");
        let mut tmp_file = File::create(file_path)?;
        writeln!(tmp_file, "Hello, this is the simulated file content!")?;
        let _ = tmp_file.set_len(42);

        let app = Router::new().nest_service(
            "/public",
            ServeDir::new(tmp_dir.path()).with_buf_chunk_size(1024 * 1024 * 10 /* 10 MiB*/),
        );

        // Simulating a GET request
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/public/file.txt")
                    .body(Body::empty())?,
            )
            .await?;

        // Validate Response Status Code
        assert_eq!(response.status(), StatusCode::OK);

        // Validate Response Headers
        assert_eq!(response.headers().get("Content-Length").unwrap(), "42");

        let _ = tmp_dir.close();

        Ok(())
    }
}
