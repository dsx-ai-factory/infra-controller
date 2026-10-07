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

//! Firmware-object retrieval boundary for automatic rack maintenance.
//!
//! Fetching is separate from the state handler so callers can provide the
//! production HTTP client or a deterministic implementation for tests.

use std::time::Duration;

use async_trait::async_trait;

/// Firmware objects contain metadata, not firmware binaries. This generous
/// limit bounds state-controller memory if the configured endpoint is wrong or
/// compromised.
const MAX_FIRMWARE_OBJECT_BYTES: usize = 16 * 1024 * 1024;

/// Loads a rack-profile SOT firmware-object document.
#[async_trait]
pub trait FirmwareObjectFetcher: std::fmt::Debug + Send + Sync {
    /// Fetches the complete document body from `url` within `timeout`.
    ///
    /// A successful call returns the body unchanged. JSON validation belongs
    /// to the state handler that consumes the document.
    ///
    /// # Errors
    ///
    /// Returns an error message when the URL cannot be fetched or its response
    /// cannot be read.
    async fn fetch(&self, url: &str, timeout: Duration) -> Result<String, String>;
}

#[async_trait]
impl FirmwareObjectFetcher for reqwest::Client {
    async fn fetch(&self, url: &str, timeout: Duration) -> Result<String, String> {
        let mut response = self
            .get(url)
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| {
                format!(
                    "failed to fetch configured SOT firmware object: {}",
                    error.without_url()
                )
            })?
            .error_for_status()
            .map_err(|error| {
                format!(
                    "configured SOT firmware object returned an unsuccessful HTTP status: {}",
                    error.without_url()
                )
            })?;

        if response
            .content_length()
            .is_some_and(|length| length > MAX_FIRMWARE_OBJECT_BYTES as u64)
        {
            return Err(format!(
                "configured SOT firmware object exceeds the {MAX_FIRMWARE_OBJECT_BYTES}-byte size limit"
            ));
        }

        let mut body = Vec::new();

        while let Some(chunk) = response.chunk().await.map_err(|error| {
            format!(
                "failed to read configured SOT firmware object: {}",
                error.without_url()
            )
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_FIRMWARE_OBJECT_BYTES {
                return Err(format!(
                    "configured SOT firmware object exceeds the {MAX_FIRMWARE_OBJECT_BYTES}-byte size limit"
                ));
            }

            body.extend_from_slice(&chunk);
        }

        String::from_utf8(body).map_err(|error| {
            format!("configured SOT firmware object body is not valid UTF-8: {error}")
        })
    }
}
