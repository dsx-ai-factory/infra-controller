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

pub const UFM_MOCK_AUTH_TOKEN_ENV: &str = "UFM_MOCK_AUTH_TOKEN";

/// Authentication token passed separately from non-secret UFM mock configuration.
///
/// The type deliberately does not implement `Debug`, `Display`, or `AsRef<str>`. Code that needs
/// the token value must explicitly call [`Self::expose_secret`].
pub struct UfmAuthToken(String);

impl UfmAuthToken {
    /// Reads the token from [`UFM_MOCK_AUTH_TOKEN_ENV`].
    pub fn from_environment() -> eyre::Result<Self> {
        let token = std::env::var(UFM_MOCK_AUTH_TOKEN_ENV).map_err(|error| {
            eyre::eyre!("could not read {UFM_MOCK_AUTH_TOKEN_ENV} environment variable: {error}")
        })?;
        Self::new(token)
    }

    pub fn new(token: String) -> eyre::Result<Self> {
        eyre::ensure!(
            !token.is_empty(),
            "UFM authentication token must not be empty"
        );
        Ok(Self(token))
    }

    /// Explicitly exposes the token to code that must construct an authorization header.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}
