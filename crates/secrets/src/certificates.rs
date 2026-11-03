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
use async_trait::async_trait;

use crate::SecretsError;

/// Build a machine URI SAN from the site trust domain, base path, and machine ID.
/// Leading and trailing slashes around the base path are normalized.
pub fn machine_spiffe_uri(trust_domain: &str, machine_base_path: &str, machine_id: &str) -> String {
    let base = machine_base_path.trim().trim_matches('/');
    if base.is_empty() {
        format!("spiffe://{trust_domain}/{machine_id}")
    } else {
        format!("spiffe://{trust_domain}/{base}/{machine_id}")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Certificate {
    pub issuing_ca: Vec<u8>,
    pub private_key: Vec<u8>,
    pub public_key: Vec<u8>,
}

#[async_trait]
pub trait CertificateProvider: Send + Sync {
    async fn get_certificate(
        &self,
        unique_identifier: &str,
        alt_names: Option<String>,
        ttl: Option<String>,
    ) -> Result<Certificate, SecretsError>;
}
